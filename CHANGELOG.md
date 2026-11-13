# Changelog

<!-- next-version-placeholder -->

## v0.18.0 (2026-11-13)

### Features
* **notifications:** add keyboard navigation to notification panel
* **smart-home:** manage device imports and cloud options in settings
* **battery:** accept more theme colours for thresholds
* **doctor:** give the agent release history
* **feedback:** add optional screenshot attachments
* **updates:** show live mirror test progress
* **updates:** support Linux Mint update management
* **website:** refresh Tileception site icons
* **session:** show Tileception logo in fastfetch
* **updates:** add multi-source update management

### Fixes
* **handoff:** avoid overwriting files with duplicate names
* **greeter:** keep sessions with an empty TryExec
* **notify:** preserve caller-supplied replacement IDs
* **power:** suspend when the lock is switched off
* **apps:** ignore empty TryExec values
* **apps:** honor default associations regardless of group order
* **calc:** evaluate percentages of powers correctly
* **windows:** keep requested size within layout slot
* **xwayland:** offset window placement by output origin
* **fastfetch:** show text logo and system modules
* **settings:** read shared Wayland keymaps from offset zero
* **updates:** fit update window controls to narrow areas
* **floats:** keep floating windows inside their area
* **ui:** center badge labels by their visible ink
* **clipboard:** paste arbitrary X11 selection types

### Documentation
* **smart-home:** document settings-based device setup
* **usage:** explain feedback screenshots

## v0.17.0 (2026-11-12)

### Features
* **wallpaper:** reload images when files change
* **outputs:** restore windows when displays return

## v0.16.1 (2026-11-11)

### Fix
* **session:** keep the SSH agent socket stable

## v0.16.0 (2026-11-11)

### Features
* **updates:** run package tasks through a polkit helper
* **updates:** preview and manage package transactions
* **theme:** add configurable primary button styles
* **ui:** unify button styling and accent treatment
* **text-input:** unify field editing and pointer selection

### Fixes
* **ui:** refine accent highlights and focus styling
* **updates:** preserve apt package state and refresh scans
* **launcher:** place submit button beside lone prompt field

### Documentation
* **updates:** explain transaction previews and maintenance

## v0.15.0 (2026-11-10)

### Features
* **calc:** expand offline unit conversions
* **plugins:** add interactive launcher prompts
* **timers:** add bundled timers and alarms
* **plugins:** add launcher cards
* **panel:** allow rearranging plugin blocks
* **updates:** add APT update manager
* **network:** add NetworkManager controls and Settings page

### Fixes
* **floating:** keep floating windows visible and on top
* **settings:** keep a section in place when Settings edits it
* **input:** end a drag when the button is released over a panel
* **state:** exit fullscreen before focusing a new window
* **handoff:** preserve pause state across session locks
* **layout:** do not panic resizing between two squeezed tiles

### Documentation
* **plugins:** document launcher cards and timers

## v0.14.1 (2026-11-10)

### Fixes
* **paste:** keep handoff pipes in blocking mode
* **render:** fit size-capped windows to the output (#63)

## v0.14.0 (2026-11-10)

### Features
* **tools:** add direct cargo-plane binary fallback
* **doctor:** use agents that are already installed
* **doctor:** add /report to send feedback from Doctor
* **clipboard:** ship the persistence helper
* **plugins:** support reloads/rediscover via `msg`
* **plugins:** stream launcher search results

### Fixes
* **launcher:** complete a bare "~" to the home folder
* **install:** report cargo-plane download failures clearly
* **duck:** schedule redraws around animation changes
* **doctor:** restart an agent that does not stop when asked
* **settings:** stop scaling the Settings window twice

### Documentation
* **config:** clarify defaults and add configuration examples

## v0.13.0 (2026-11-08)

### Features
* **settings:** manage default applications
* **greeter:** show session logos
* **plugins:** add bar chips and bundled Bluetooth controls
* **bluetooth:** add device controls and reconnect support
* **plugins:** add bundled plugins and panel resources
* **plugins:** add shared plugin host
* **smart-home:** add guided device setup in settings
* **gestures:** make navigation follow touchpad input

### Fixes
* **terminal:** honor terminal and MIME application preferences
* **session:** enable Secret Service integration for desktop apps

### Performance
* **build:** speed up development installs

### Documentation
* **plugins:** document bundled and panel plugins

## v0.12.0 (2026-11-07)

### Features
* **feedback:** add issue preview and confirmation
* **launcher:** add executable plugin support
* **displays:** add output previews and responsive Settings
* **doctor:** add permission modes and document export
* **website:** improve metadata and favicon assets
* **compositor:** add feedback and notification commands
* **settings:** host Settings in an in-process Wayland client
* **outputs:** add proportional screen-edge crossing
* **settings:** add native settings screens
* **settings:** add interactive settings panel

### Fixes
* **outputs:** improve display configuration and transformed input
* **feedback:** improve form editing and review flow
* **launcher:** improve plugin lifecycle and action results
* **doctor:** resync agent permission mode before prompts
* **displays:** handle output preview state

### Documentation
* **plan:** record settings panel progress

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
