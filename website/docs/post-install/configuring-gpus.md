# How to configure GPUs

If a Mac has a dGPU, it boots from it and uses it as the primary display
adapter by default. Which GPU options KAIT2EN offers depends on the model:

- **MacBookPro15,1, MacBookPro16,1 and MacBookPro16,4** support hybrid
  graphics through **T2 Hybrid GPU Control**.
- **Other MacBook Pro models with a dGPU** can switch between iGPU and dGPU
  through **T2 GPU Control**.
- **iMacs** cannot switch, because there are no display lines from the iGPU to
  the panel. The iGPU is only used for offloading, and KAIT2EN installs no GPU
  app. The 5K iMacs (iMac20,1 and iMac20,2) still get a patched AMDGPU module,
  which carries their 5K panel support.
- **Mac Pros** have no iGPU, so there is nothing to configure.

## MacBookPro15,1, MacBookPro16,1 and MacBookPro16,4: enable hybrid graphics

KAIT2EN installs **T2 Hybrid GPU Control** on these models. Open it from the
application menu and enable **Hybrid graphics**, then reboot.

Hybrid graphics makes the integrated GPU the display GPU. Applications can
still use the AMD GPU through PRIME offload. The kernel wakes it automatically
for accelerated work and returns it to D3cold when it becomes idle. This keeps
the dGPU available without paying its idle power cost. System suspend and
resume are fully supported in hybrid mode on all three models.

Hybrid graphics needs the patched AMDGPU module described below. The app
reports whether its runtime-PM support is active.

The discrete-GPU boot option remains available as a recovery setting. Rebooting
is always a separate action so changing the stored boot GPU does not restart the
system unexpectedly.

## The patched AMDGPU module and kernel updates

On the three MacBook Pro models above and on the 5K iMacs, the installer builds
a patched AMDGPU module for the current Fedora kernel. It carries hybrid
runtime PM for the MacBook Pros and the 5K panel support for the iMacs.

This module is not managed by DKMS, but the installer adds a kernel-install
hook that rebuilds it for every kernel Fedora installs later. The build runs
during the kernel update, after DKMS and before the initramfs is generated, so
the new kernel boots with the patched module. It downloads the kernel's source
RPM, so it needs network access and adds a few minutes to the update. dnf does
not show its output. It is written to `/var/log/kait2en-gpu-runtime-pm.log`.

If the build fails, for example without network access, the update itself still
completes and the new kernel boots with Fedora's stock AMDGPU, without hybrid
runtime PM or 5K support. Rebuild it for that kernel and reboot:

```bash
sudo /usr/local/libexec/kait2en/gpu-runtime-pm/install-gpu-runtime-pm.sh install <kernel>
sudo reboot
```

## Other MacBook Pro models with a dGPU

Other Intel/AMD MacBook Pro models use **T2 GPU Control**. Hybrid runtime PM is
not enabled on those models because their dGPU power-on path is not yet
reliable. **T2 GPU Control** has options to switch between iGPU and
dGPU and to enable power saving for the dGPU. For the changes to take effect
you need to reboot.
Usually, users prefer iGPU as primary to save some energy and make suspend
more reliable.
