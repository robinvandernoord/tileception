# Changelog

<!-- next-version-placeholder -->

## v0.11.0 (2026-11-07)

### Features
* new `frost` theme
* **handoff:** carry colour picks across screens and machines
* **duck:** support copy, cut, and paste in the question field
* **compositor:** add launcher tools and keyboard notification actions
* **clipboard:** add configurable clipboard persistence
* **home:** add Tuya smart home controls and diagnostics (#16)
* **bar:** improve notification badge and calendar popup
* **input:** add key repeat and greeter text selection

### Fix
* **setup:** detect config-capable tileception binary

### Documentation
* **usage:** describe colour pick handoff
* **doctor:** document smart home approvals
* **workflow:** document text fields and future plans

## v0.10.0 (2026-11-02)

### Features
* **outputs:** split screens into independent areas
* **outputs:** resize and persist split screen areas
* **split-screen:** extend saved views and minimap across areas
* **minimap:** add interactive layout map

### Documentation
* **split-screen:** document saved views across areas
* **plan:** update release status and outline screen splitting

## v0.9.0 (2026-11-02)

### Features
* **handoff:** improve cross-machine window handling with multi-output support, Xwayland screen stand-ins, X11 overlays, remote dialogs, and clipboard file transfers
* **handoff:** improve input forwarding, keyboard state synchronization, and selection sharing across machines
* **release:** publish Debian prereleases to the `pre` component
* **screensaver:** honor D-Bus idle inhibitors
* **power:** add scheduled wake support
* **portal:** support KeePassXC auto-type

### Fixes
* **bar:** suppress GL driver errors in error notifications
* **remote-desktop:** resolve non-dumpable session executables
* **render:** display X11 overlays over fullscreen windows

### Documentation
* **handoff:** document headless handoff testing
* **README:** add promotional video

## v0.8.1 (2026-10-29)

### Fixes
* **setup:** unlock keyrings through greetd PAM
* **greeter:** write greeter logs to a file

### Documentation
* **plan:** outline machine handoff

## v0.8.0 (2026-10-29)

### Features
* **commands:** report image cache usage
* **setup:** add guided desktop and greeter setup
* **greeter:** add layout picker and richer login panels
* **doctor:** bundle user guides for the assistant
* **shell:** support positioning the built-in bar at the bottom
* **greeter:** add login screen setup and testing commands
* **greeter:** add login screen and setup tools
* **session:** share the SSH agent with graphical apps

### Fixes
* **greeter:** restore the previous greetd configuration
* **screenshot:** ask before saving or copying captures
* **render:** synchronize frames and cursor callbacks
* **ui:** improve launcher and compact-screen controls

### Documentation
* **usage:** update setup and feature guides
* info about new greeter config options
* **project:** add user guides and refresh project information
* tweaked planned greeter design

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
