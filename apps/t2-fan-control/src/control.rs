use std::time::{Duration, Instant};

use crate::{
    config::{curve, curve_percent, AppConfig, CurvePoint},
    error::Result,
    sysfs::{FanEndpoint, TemperatureSnapshot, TemperatureSource},
};

const CONTROL_INTERVAL: Duration = Duration::from_secs(2);
const SYSTEM_LIMIT_HYSTERESIS_C: u8 = 2;
const SYSTEM_STABLE_RELEASE_TIME: Duration = Duration::from_secs(20);
const RAMP_UP_PERCENT_PER_TICK: u8 = 8;
const RAMP_DOWN_PERCENT_PER_TICK: u8 = 4;
const RAMP_START_THRESHOLD_PERCENT: u8 = 3;

pub struct Controller {
    last_applied_percent: Option<u8>,
    ramping: bool,
    last_tick: Instant,
    heat_soak_cooling: bool,
    any_sensor_cooling: bool,
    system_cooling_started_at: Option<Instant>,
    system_below_target_since: Option<Instant>,
}

#[derive(Clone, Debug, Default)]
pub struct ControlSnapshot {
    pub temperatures: TemperatureSnapshot,
    pub effective_temp_c: Option<u8>,
    pub system_temp_c: Option<u8>,
    pub system_sensor_count: usize,
    pub heat_soak_cooling: bool,
    pub any_sensor_cooling: bool,
    pub target_percent: Option<u8>,
    pub target_rpm_per_fan: Vec<u32>,
}

impl Controller {
    pub fn new() -> Self {
        Self {
            last_applied_percent: None,
            ramping: false,
            last_tick: Instant::now() - CONTROL_INTERVAL,
            heat_soak_cooling: false,
            any_sensor_cooling: false,
            system_cooling_started_at: None,
            system_below_target_since: None,
        }
    }

    pub fn tick(
        &mut self,
        config: &AppConfig,
        fans: &mut [FanEndpoint],
        temperatures: &mut [TemperatureSource],
        refresh_all_sensors: bool,
    ) -> Result<ControlSnapshot> {
        let mut snapshot = TemperatureSnapshot::read_for_control(
            temperatures,
            config.curve_sensor_key.as_deref(),
            refresh_all_sensors,
        );
        snapshot.include_curve_sensor(temperatures, config.curve_sensor_key.as_deref());
        let effective_temp = snapshot.effective_temp_c();

        let curve = curve(config);
        let curve_target = effective_temp.map(|temp| interpolate_percent(&curve, temp));
        self.update_system_cooling(config, snapshot.system_temp_c);
        self.any_sensor_cooling = if config.any_sensor_enabled {
            next_threshold_cooling(
                self.any_sensor_cooling,
                snapshot.overall_hottest_temp_c,
                config.any_sensor_temp_c,
                config.any_sensor_temp_c.saturating_sub(SYSTEM_LIMIT_HYSTERESIS_C),
            )
        } else {
            false
        };
        let above_curve = effective_temp
            .zip(curve.last())
            .is_some_and(|(temp, end)| temp > end.temp_c);
        let forced = self.heat_soak_cooling || self.any_sensor_cooling || above_curve;
        let mut target_percent = if forced { Some(100) } else { curve_target };

        let mut target_rpm_per_fan = Vec::with_capacity(fans.len());
        if config.automatic_control_enabled {
            let (next_percent, ramping) = if forced {
                (target_percent, false)
            } else {
                ramp_step(self.last_applied_percent, target_percent, self.ramping)
            };
            let should_apply = next_percent != self.last_applied_percent;
            self.ramping = ramping;
            target_percent = next_percent;

            for fan in fans {
                let rpm = target_percent
                    .map(|percent| fan.percent_to_rpm(percent))
                    .unwrap_or(fan.min_speed);

                if should_apply {
                    fan.set_target_speed(rpm)?;
                    fan.current_speed = Some(rpm);
                    fan.app_controlled = Some(true);
                }

                target_rpm_per_fan.push(rpm);
            }

            if should_apply {
                self.last_applied_percent = target_percent;
            }
        }

        self.last_tick = Instant::now();

        let system_temp_c = snapshot.system_temp_c;
        let system_sensor_count = snapshot.system_sensor_count;
        Ok(ControlSnapshot {
            temperatures: snapshot,
            effective_temp_c: effective_temp,
            system_temp_c,
            system_sensor_count,
            heat_soak_cooling: self.heat_soak_cooling,
            any_sensor_cooling: self.any_sensor_cooling,
            target_percent,
            target_rpm_per_fan,
        })
    }

    pub fn release_to_system(&mut self, fans: &mut [FanEndpoint]) -> Result<()> {
        for fan in fans {
            fan.release_to_auto()?;
            fan.app_controlled = Some(false);
        }
        self.last_applied_percent = None;
        self.ramping = false;
        self.heat_soak_cooling = false;
        self.any_sensor_cooling = false;
        self.system_cooling_started_at = None;
        self.system_below_target_since = None;
        Ok(())
    }

    pub fn should_tick(&self) -> bool {
        self.last_tick.elapsed() >= CONTROL_INTERVAL
    }

    fn update_system_cooling(&mut self, config: &AppConfig, system_temp_c: Option<u8>) {
        let now = Instant::now();
        let engage = config.soak_temp_c.saturating_add(SYSTEM_LIMIT_HYSTERESIS_C);
        let release = config.soak_temp_c.saturating_sub(SYSTEM_LIMIT_HYSTERESIS_C);

        if !self.heat_soak_cooling {
            if system_temp_c.is_some_and(|temp| temp >= engage) {
                self.heat_soak_cooling = true;
                self.system_cooling_started_at = Some(now);
                self.system_below_target_since = None;
            }
            return;
        }

        if system_temp_c.is_some_and(|temp| temp <= release) {
            self.system_below_target_since.get_or_insert(now);
        } else {
            self.system_below_target_since = None;
        }

        let minimum_elapsed = self.system_cooling_started_at
            .is_some_and(|started| now.duration_since(started) >= Duration::from_secs(config.system_cooling_time_s as u64));
        let stable_elapsed = self.system_below_target_since
            .is_some_and(|started| now.duration_since(started) >= SYSTEM_STABLE_RELEASE_TIME);
        if minimum_elapsed && stable_elapsed {
            self.heat_soak_cooling = false;
            self.system_cooling_started_at = None;
            self.system_below_target_since = None;
        }
    }

}

fn next_threshold_cooling(active: bool, temp_c: Option<u8>, engage_temp_c: u8, release_temp_c: u8) -> bool {
    match temp_c {
        Some(temp) if temp >= engage_temp_c => true,
        Some(temp) if temp <= release_temp_c => false,
        _ => active,
    }
}

/// Moves the applied speed toward the target by a limited step per tick. A new ramp only
/// starts once the target differs by the threshold; a running ramp continues to the target.
fn ramp_step(applied: Option<u8>, target: Option<u8>, ramping: bool) -> (Option<u8>, bool) {
    let (Some(applied), Some(target)) = (applied, target) else {
        return (target, false);
    };
    if !ramping && applied.abs_diff(target) < RAMP_START_THRESHOLD_PERCENT {
        return (Some(applied), false);
    }
    let next = if target > applied {
        applied.saturating_add(RAMP_UP_PERCENT_PER_TICK).min(target)
    } else {
        applied.saturating_sub(RAMP_DOWN_PERCENT_PER_TICK).max(target)
    };
    (Some(next), next != target)
}

fn interpolate_percent(curve: &[CurvePoint], temp_c: u8) -> u8 {
    curve_percent(curve, temp_c as f64).round().clamp(0.0, 100.0) as u8
}

#[cfg(test)]
mod tests {
    use super::{interpolate_percent, next_threshold_cooling, ramp_step};
    use crate::config::CurvePoint;

    #[test]
    fn system_target_has_plus_minus_two_degree_hysteresis() {
        assert!(next_threshold_cooling(false, Some(47), 47, 43));
        assert!(next_threshold_cooling(true, Some(45), 47, 43));
        assert!(next_threshold_cooling(true, Some(44), 47, 43));
        assert!(!next_threshold_cooling(true, Some(43), 47, 43));
    }

    #[test]
    fn temperature_above_last_curve_point_forces_full_speed() {
        let curve = vec![
            CurvePoint { temp_c: 0, speed_percent: 0 },
            CurvePoint { temp_c: 70, speed_percent: 30 },
            CurvePoint { temp_c: 90, speed_percent: 50 },
        ];
        assert_eq!(interpolate_percent(&curve, 90), 50);
        assert_eq!(interpolate_percent(&curve, 91), 100);
        assert_eq!(interpolate_percent(&curve, 100), 100);
    }

    #[test]
    fn ramp_limits_step_per_tick_and_finishes_at_target() {
        assert_eq!(ramp_step(Some(20), Some(40), false), (Some(28), true));
        assert_eq!(ramp_step(Some(36), Some(40), true), (Some(40), false));
        assert_eq!(ramp_step(Some(40), Some(20), false), (Some(36), true));
        assert_eq!(ramp_step(Some(22), Some(20), true), (Some(20), false));
    }

    #[test]
    fn ramp_ignores_small_changes_while_settled() {
        assert_eq!(ramp_step(Some(20), Some(22), false), (Some(20), false));
        assert_eq!(ramp_step(None, Some(22), false), (Some(22), false));
        assert_eq!(ramp_step(Some(20), None, false), (None, false));
    }
}
