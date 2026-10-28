# Changelog

<!-- next-version-placeholder -->

## v0.7.0 (2026-10-18)

### Feature
* **tray:** add StatusNotifier icons to the built-in bar

### Fix
* **autostart:** launch XDG desktop entries at session start

### Documentation
* **greeter:** add interactive design prototype
* **install:** plan guided setup flow
* **build:** document cargo-plane setup and builder workflows

## v0.6.0 (2026-10-18)

### Feature
* **duck:** add interactive screenshot assistant

### Fixes
* **focus:** defer keyboard focus until initial configure
* **event-loop:** flush client events promptly
* **shapes:** reuse stable elements across frames

### Documentation
* **scale:** explain fractional screen scaling

## v0.5.0 (2026-10-17)

### Features
* **cli:** add help and version output
* **doctor:** add copy and run controls to code blocks
* **compositor:** add input gestures and power management

### Fixes
* **windows:** handle maximize requests after initial configure
* **acp:** support Bun as an npx runner

### Documentation
* **project:** refresh architecture and roadmap

## v0.4.0 (2026-10-17)

### Features
* **packaging:** manage the apt keyring as a package
* **launcher:** support per-app launch arguments

### Documentation
* **plan:** update packaging and launcher progress

## v0.3.0 (2026-10-17)

### Features
* **shell:** add smart launcher and built-in top bar
* **tileception:** add HiDPI and fractional output scaling

### Documentation
* **plan:** record launcher and top bar implementation
* **plan:** record HiDPI scaling implementation

## v0.2.0 (2026-10-16)

### Features
* **overlays:** respect overlay surfaces across panels and input
* **packaging:** add Debian sid package support
* **launcher:** support clipboard paste and media change notices
* **doctor:** add agent login, image, and session controls
* **session:** add login screen badge
* **doctor:** add ACP-powered setup assistant
* **desktop:** add configurable shell and compositor integrations
* **media:** add built-in volume, brightness, and playback controls
* **windows:** add minimize and restore support
* **windows:** keep slightly oversized tiled windows live
* **portal:** serve desktop appearance settings

### Fixes
* **website:** use relative paths for local previews
* **dbus:** pass session variables to activation environment
* **compositor:** harden media controls and window handling
* **launcher:** show home when no windows remain in the layout
* **floats:** expand floating windows when their content grows
* **wayland:** support dmabuf screencopy and wait for active seat

### Documentation
* **plan:** record launcher and app launch proposals
* **plan:** propose top bar and smarter launcher
* **plan:** mark media controls implemented
* **website:** label releases as alpha

## v0.1.0 (2026-10-16)

### Features
* **release:** publish Debian packages and add apt installer
* **website:** add site and deployment workflow
* **packaging:** add Debian package builds
* **ui:** refresh compositor surfaces and interactions
* **power:** enable suspend by default
* **layout:** support exact selection framing and local splits
* **power:** make suspend opt-in for resume testing
* **session:** integrate built-in desktop features
* **notify:** add private D-Bus notification service
* **auth:** add Linux PAM authentication crate
* **apps:** add desktop application and icon index
* **compositor:** add Tide theme, text, wallpaper, and power menu
* **tileception:** add Wayland compositor and layout engine

### Fixes
* **release:** verify published Debian package downloads
* **launcher:** use compositor clipboard transfers

### Documentation
* **project:** document layout, clipboard, and suspend updates
* **session:** document built-in desktop features
