# Independent DSP package

`t2-dsp` packages host-side audio processing, not a bridgeOS service or a
kernel module. RPM and Debian builds contain all 13 model profiles and require
neither T2 hardware nor network access. No compiled plugins are bundled.

## Build

From the repository root, create a source archive:

```sh
bash dsp/packaging/source-archive.sh /tmp/t2-dsp-0.1.0.tar.gz
```

The archive contains `dsp/`, `packaging/lifecycle/`, `LICENSE`, `LICENSING.md` and `LICENSES/` under
`t2-dsp-0.1.0/`. Generated files and Python caches are excluded.

For RPM, put the archive in the builder's `SOURCES/` directory and run:

```sh
rpmbuild -ba dsp/packaging/rpm/t2-dsp.spec
sudo dnf install ./t2-dsp-0.1.0-1*.noarch.rpm
```

For Debian/Ubuntu, unpack the archive, copy `dsp/packaging/debian/` to
`debian/` in the extracted root, and put a copy of the archive beside that
directory named `t2-dsp_0.1.0.orig.tar.gz`. Inside the source tree:

```sh
dpkg-buildpackage -us -uc
sudo apt install ../t2-dsp_0.1.0-1_all.deb
```

Once a repository publishes the package, use `dnf install t2-dsp` or
`apt install t2-dsp`. These recipes do not publish a repository.

Runtime dependencies are PipeWire with filter-chain/LV2 support, WirePlumber
0.5.8 or newer, Bankstown and LSP LV2 plugins. Fedora calls Bankstown
`lv2-bankstown`, Debian/Ubuntu packaging uses `bankstown-lv2`.
The old installer also requested Triforce and SWH, but none of the current
graphs references either. Do not silently drop a required plugin if it is
unavailable in a target distribution. Supply it through the target repository.

The driver and the AppleT2x2/x4/x6 UCM profiles must be supplied separately.
There is deliberately no guessed dependency on a future BCE stack package
name. Add that distribution-specific dependency when the stack is packaged.
WirePlumber must provide `node.software-dsp` and UCM split-PCM support.

## Validation

`make -C dsp test` runs hardware-free build, routing and migration tests.
With PipeWire and the runtime plugins installed, run
`python3 dsp/tests/smoke-pipewire.py` to load all 24 graphs and verify their
48 nodes in a private PipeWire daemon. It creates no hardware monitor and
does not connect to the user's PipeWire session or produce audio.

RPM builds, Debian 13 builds and an APT install with dependencies are tested.
Actual T2 playback, recording and headphone switching still need a hardware
test after reboot. A successful package build alone cannot establish that.

## Activation, migration and removal

No post-install script restarts user audio, triggers live card renaming, or
touches GNOME, dconf or PAM. Reboot after installation or removal. This also
ensures udev model IDs exist before ALSA/PipeWire opens the card.

Package-owned configuration lives under `/usr`, with administrator overrides
under `/etc`. The former generated `/etc` DSP and quantum fragments are
backed up to `/var/lib/kait2en/migration/t2-dsp/` before retirement. Ownership
receipts live in `/var/lib/kait2en/ownership/t2-dsp.json`. Matching former
profile data under `/usr/share/kait2en/audio-dsp/` is retired too. Removal
deletes verified, unchanged backups and the completed migration receipt.
Backups from the earlier migration helper are also adopted when their
generated-file marker is present. Modified or unrecognized files are preserved
and reported, not silently deleted. Logs remain available for diagnosis.

Lifecycle failures are collected, printed in a final summary and logged to
`/var/log/t2-dsp-install.log`. The source installer also receives these errors
in its shared ledger. An activation failure does not interrupt independent
installation steps or the package transaction. Build and staging failures,
in contrast, fail the package build so incomplete packages are not produced.

The independently shipped lifecycle engine requires Python 3. Its source
lives under `packaging/lifecycle/`, separately from `auto-installer/`.

## Licenses

The package includes the full GPL-3.0-or-later text, `LICENSE`, `LICENSING.md`
and each profile's attribution notice. The section 7(b) attribution term
continues to apply. External plugins retain their own package licenses.
The runtime-selection design originates in sharpenedblade's merged PR #63.
