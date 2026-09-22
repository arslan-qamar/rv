# Remote Viewer MVP

View-only LAN desktop streaming. A Windows Service owns the TCP listener; an agent launched at interactive logon captures the primary display using DXGI Desktop Duplication, JPEG-encodes frames, and sends them to the service over loopback. The Flutter viewer runs on Linux and Android. There are no input-control protocol messages or remote-control handlers.

**LAN DEVELOPMENT VERSION. Authentication traffic is not encrypted. Do not expose port 5901 to the public internet.**

## Build

On a Windows 10/11 build machine, install the Rust toolchain with the MSVC target, Visual Studio C++ build tools, and NSIS. Run `powershell -ExecutionPolicy Bypass -File .\build-windows.ps1` from this repository. The output is `installer\RVHostSetup.exe`.

The checked-in source can also be cross-built on Linux with Rust's `x86_64-pc-windows-gnu` target, MinGW and NSIS: build `host` with `cargo build --release --target x86_64-pc-windows-gnu`, then run `makensis -DBUILDTARGET=x86_64-pc-windows-gnu installer/RVHost.nsi`.

On a Linux machine with Flutter and the Linux desktop development dependencies, run:

```sh
cd viewer
flutter pub get
flutter build linux
flutter build apk
```

The Linux executable and its shared libraries are under `viewer/build/linux/x64/release/bundle`. The Android APK is under `viewer/build/app/outputs/flutter-apk/app-release.apk`.
For this prototype, the Android release build uses Flutter's generated debug signing key.

The Android manifest in this repository grants Internet access for LAN TCP connections. The generated platform scaffolding supplies `MainActivity` and desktop launcher files.

## CI/CD and downloads

GitHub Actions builds all three targets on pull requests, pushes to `main`, and manual runs. Open a successful **Actions > CI** run and download its artifacts to test the Windows installer, Linux bundle, or debug-signed Android APK. CI artifacts are retained for 14 days and are intended for testing, not permanent distribution.

Pushing a semantic version tag creates a permanent GitHub Release containing:

- `RVHost-Windows-x64-Setup.exe`
- `RemoteViewer-Linux-x64.tar.gz`
- `RemoteViewer-Android.apk`
- `SHA256SUMS.txt`

The Android release asset is intentionally a debug build signed with Flutter's generated debug key because this is a personal-distribution tool. It is suitable for direct sideloading, but not for Google Play publishing or a broadly distributed production app.

Create and publish a release with:

```sh
git tag v0.1.0
git push origin v0.1.0
```

The same workflow can be started manually for an existing `vMAJOR.MINOR.PATCH` tag. Release downloads appear at `https://github.com/arslan-qamar/rv/releases/latest`; stable direct links can use `/releases/latest/download/<asset-name>`.

For this project, GitHub Releases are the best publishing option: assets are attached to version tags, remain available until removed, and have stable download links. The other GitHub-hosted choices serve different purposes:

- **Actions artifacts** are useful for branch and pull-request testing, but they expire and downloaders need repository read access.
- **GitHub Pages** can provide a friendly download website that links to Release assets; it should not be the binary store itself.
- **GitHub Packages** is designed for package registries and container images, not desktop installers or standalone APK downloads.
- **Committing binaries or using Git LFS** makes repository history and cloning heavier; Releases are a better fit for generated installers.

## Install and use

Run the Windows installer as an administrator, enter a device name, port and password, then finish. The installer writes an Argon2id password hash in `password.hash` with service/admin access and a random local agent token in `config.json` with interactive-user read access, both under `%PROGRAMDATA%\RVHost`. It creates an automatic `RVHost` service, registers `RVCapture` under the machine-wide Run key, adds an inbound Windows Firewall rule for the configured TCP port, and starts both components. The rule applies to every Windows network profile so VM adapters work, while limiting remote addresses to the local subnet. `RVCapture` launches again at each interactive logon.

Open the viewer and it will scan the local network for RVHost services. Select a discovered device, enter its password, and connect. Manual host and port entry remains available for networks where multicast discovery is blocked. One viewer can connect at a time. On Android, pinch and pan affect only the local image.

The capture agent also takes a screenshot about once a minute while a Windows desktop session is available, including when no viewer is connected. During live viewing, the service saves at most one frame per minute. Open **Saved screenshots** in the connected viewer to browse them. The service keeps the encrypted files in `%PROGRAMDATA%\RVHost\screenshots`, deleting the oldest files when the archive reaches 5% of the capacity of that volume. The files use AES-256-GCM with a key derived from the viewing password using Argon2id. Windows DPAPI protects a copy of that key so the service can keep capturing after a reboot without a viewer login. The screenshot directory and protected key are restricted to SYSTEM and administrators. Reinstalling with a different password clears the old archive.

The viewer receives decrypted screenshots after authentication. As with live frames, the current LAN protocol does not encrypt traffic in transit; use this only on a trusted local network.

The installed service executable is `RVHost.exe`, and the interactive capture-agent executable is `RVCapture.exe`. The service advertises `_rvhost._tcp.local.` over mDNS/UDP 5353 on the local subnet; discovery normally does not cross routers, VLANs, guest-network isolation, or multicast-blocking VPNs. If the viewer authenticates but remains at “waiting for Windows desktop,” confirm `RVCapture.exe` is running in the logged-in user's Task Manager session. Capture and IPC errors are written to `%LOCALAPPDATA%\RVHost\agent.log`. Service, viewer-session, and discovery errors are written to `%PROGRAMDATA%\RVHost\host.log`.

## Protocol v1

Each message has a 6-byte header: version `u8` (1), type `u8`, and payload length `u32` big endian. Maximum payload is 16 MiB. Viewer types: `AUTH=1`, `AUTH_SUCCESS=2`, `AUTH_FAILURE=3`, `SCREEN_INFO=4`, `FRAME=5`, `PING=6`, `PONG=7`, `BUSY=8`, `DISCONNECT=9`, `SNAPSHOT_LIST=10`, `SNAPSHOT_LIST_REPLY=11`, `SNAPSHOT_GET=12`, `SNAPSHOT_FRAME=13`, `SNAPSHOT_ERROR=14`. Authentication is a raw UTF-8 password payload. `SCREEN_INFO` contains width and height as big-endian `u32`. `FRAME` and `SNAPSHOT_FRAME` contain frame ID `u64`, width `u32`, height `u32`, JPEG length `u32`, then JPEG bytes. Screenshot lists contain up to 1000 most recent IDs as big-endian `u64` values. Agent IPC uses the same envelope on 127.0.0.1:45901, with a random token in its `AUTH` message.

The service stores a single latest frame; it replaces stale frames when capture outruns a viewer. No frame queue grows over time. A slow TCP write can still delay one frame for up to the three-second write timeout. The service tells the local capture agent to stream after a viewer authenticates and to stop streaming after that viewer disconnects. While idle, the agent releases DXGI resources between periodic capture requests. During an active session it drops byte-identical frames before color conversion and JPEG encoding.

## Current validation boundary

The Rust host cross-compiled for Windows, the NSIS installer compiled, and the protocol and screenshot encryption unit tests passed. Flutter was not available in this development environment for validation of the new viewer UI. This Linux environment cannot run the Windows Service, DXGI capture, installer, or Android application. Install on a Windows 10/11 host and check periodic captures while disconnected, screenshot browsing from Linux and Android, archive rotation, password failure, second-viewer `BUSY`, display resolution change, logout and login, and viewer disconnect/reconnect.
