# Harmonic on Boox Go 10.3 via Termux

Manual sync on launch: tapping the widget shortcut runs one client sync and
exits. No daemon, no background service. The client binary is a plain Android
(aarch64) executable that runs inside Termux.

## Setup

On the Boox:

1. Install [Termux](https://f-droid.org/en/packages/com.termux/),
   [Termux:Widget](https://f-droid.org/en/packages/com.termux.widget/) and
   [Termux:API](https://f-droid.org/en/packages/com.termux.api/) (F-Droid
   builds only, do not mix with Play Store builds). Also install the
   Termux:API companion app package inside Termux: `pkg install termux-api`,
   plus `pkg install termux-media-scan` for library refresh.
2. Grant shared storage access: `termux-setup-storage`
3. Create the working directory and download the client from the release
   page:
   ```
   mkdir -p ~/harmonic/.harmonic
   cd ~/harmonic
   curl -LO <release-url>/harmonic-client-aarch64-linux-android
   mv harmonic-client-aarch64-linux-android harmonic-client
   chmod +x harmonic-client
   ```
4. Create `~/harmonic/.harmonic/config.toml`. The sync path points at the
   shared storage folder your books live in:
   ```toml
   sync_path = '/storage/emulated/0/Books/harmonic'
   socket_addr = '<desktop server ip>:42069'
   schedule_delay = 3600
   log_level = 'info'
   sync_threshold = 20
   modify_weight = 2
   remove_weight = 5
   create_weight = 10
   block_size = 8192
   ```
5. Copy the widget scripts into `~/.shortcuts/`:
   ```
   cp scripts/termux/harmonic-sync.sh scripts/termux/harmonic-bootstrap.sh ~/.shortcuts/
   chmod +x ~/.shortcuts/harmonic-*.sh
   ```
   They now appear in the Termux:Widget launcher.

On the desktop (one time, while both devices are on the same network):

1. Run `harmonic-server --bootstrap` and note the printed OTP.
2. On the Boox, tap the **harmonic-bootstrap** widget shortcut and enter the
   OTP (Termux:API dialog). This downloads the server certificate.
3. Tap the **harmonic-sync** widget shortcut to sync.

Subsequent syncs: tap **harmonic-sync**. The server keeps running on the
desktop; the Boox side is invoked only when you launch it.

## Notes and limitations

- Event based sync is not supported on the Boox: the shared storage is a FUSE
  mount where inotify events are unreliable, hence manual-only.
- Termux keeps the working directory of widget scripts stable but the
  `.harmonic` state directory is resolved relative to where the client runs,
  which is why the scripts `cd` into `~/harmonic` first.
- The OTP is 64 characters; pasting it into the Termux:API dialog is easiest
  via the clipboard.
- Files synced onto the device are pushed through the Android media scanner
  (`termux-media-scan`) so reader apps pick them up. Some Boox library apps
  only index on their own schedule.
- Keep both devices NTP time synced: sync decisions compare modification
  timestamps between devices.
- The client trusts the server certificate pinned at bootstrap. If the
  desktop server's IP changes, regenerate its certificate and re-bootstrap.
