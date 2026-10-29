//! A line-based command socket, the counterpart of TrellisWM's `trelliswm://`
//! links. `tileception msg <command>` sends one command and prints the reply.

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::Duration,
};

use smithay::reexports::calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic};
use tileception_core::{Node, NodeId};

use crate::Tileception;

pub fn socket_path(wayland_display: &str) -> PathBuf {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    dir.join(format!("tileception-{wayland_display}.sock"))
}

pub fn listen(event_loop: &mut EventLoop<'static, Tileception>, path: &PathBuf) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    event_loop
        .handle()
        .insert_source(Generic::new(listener, Interest::READ, Mode::Level), |_, listener, state| {
            while let Ok((stream, _)) = listener.accept() {
                if let Err(err) = serve(state, stream) {
                    tracing::warn!("ipc: {err}");
                }
            }
            Ok(PostAction::Continue)
        })
        .map_err(|err| std::io::Error::other(err.to_string()))?;
    Ok(())
}

fn serve(state: &mut Tileception, stream: UnixStream) -> std::io::Result<()> {
    stream.set_nonblocking(false)?;
    stream.set_read_timeout(Some(Duration::from_millis(500)))?;
    let mut line = String::new();
    BufReader::new(&stream).take(4096).read_line(&mut line)?;
    let reply = match handle(state, line.trim()) {
        Ok(text) => text,
        Err(err) => format!("error: {err}\n"),
    };
    (&stream).write_all(reply.as_bytes())
}

fn handle(state: &mut Tileception, line: &str) -> Result<String, String> {
    let command = crate::commands::parse(line)?;
    state.execute(command)
}

/// A readable dump of the tree, the framed node and focus, for scripts and tests.
pub fn describe(state: &Tileception) -> String {
    let world = &state.world;
    let tree = world.tree();
    let mut out = format!(
        "framed {} overview {} focus {}\n",
        world.framed().0,
        world.in_overview(),
        world.focus().map(|window| window.0.to_string()).unwrap_or("none".into())
    );
    out.push_str(&format!(
        "locked {} display-asleep {} selected {:?}\n",
        state.is_locked(),
        state.idle.off,
        state.selection.windows.iter().map(|window_id| window_id.0).collect::<Vec<_>>()
    ));
    out.push_str(&format!("minimized {:?}\n", world.minimized().map(|window_id| window_id.0).collect::<Vec<_>>()));
    if let Some(output) = &state.output {
        out.push_str(&format!("output {} pointer {:.1},{:.1}\n", output.name(), state.pointer.x, state.pointer.y));
    }
    fn walk(state: &Tileception, node_id: NodeId, depth: usize, out: &mut String) {
        let world = &state.world;
        let tree = world.tree();
        let pad = "  ".repeat(depth);
        let rect = world
            .node_rect(node_id, world.target_view())
            .map(|bounds| format!(" @ {:.0},{:.0} {:.0}x{:.0}", bounds.x, bounds.y, bounds.w, bounds.h))
            .unwrap_or_else(|| " hidden".into());
        let line = match tree.node(node_id) {
            Node::Split { axis, weights, .. } => format!("split {} {:?} {:.2?}", node_id.0, axis, weights),
            Node::Tile { views, selected } => {
                let titles: Vec<String> = views
                    .iter()
                    .map(|window_id| {
                        let app = state.windows.get(window_id).map(crate::state::app_id).unwrap_or_default();
                        format!("{}:{}", window_id.0, app)
                    })
                    .collect();
                format!("tile {} [{}] selected {}", node_id.0, titles.join(", "), selected)
            }
            Node::Desktop { floats, shown } => {
                let float_ids: Vec<String> = floats.iter().map(|window| window.0.to_string()).collect();
                format!("desktop {} shown {} floats [{}]", node_id.0, shown, float_ids.join(", "))
            }
        };
        out.push_str(&format!("{pad}{line}{rect}\n"));
        for child in tree.children(node_id) {
            walk(state, *child, depth + 1, out);
        }
    }
    walk(state, tree.root(), 0, &mut out);
    out
}

fn only_socket() -> std::io::Result<PathBuf> {
    let dir = socket_path("x").parent().map(PathBuf::from).unwrap_or_default();
    let found: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.file_name().is_some_and(|name| name.to_string_lossy().starts_with("tileception-")))
        .filter(|path| path.extension().is_some_and(|ext| ext == "sock"))
        .collect();
    match &found[..] {
        [one] => Ok(one.clone()),
        [] => Err(std::io::Error::other("no running Tileception found")),
        _ => Err(std::io::Error::other("several Tileceptions are running; set TILECEPTION_SOCKET")),
    }
}

/// `tileception msg ...`: sends a command to the running compositor.
pub fn client(args: &[String]) -> std::io::Result<()> {
    let path = match std::env::var_os("TILECEPTION_SOCKET") {
        Some(socket) => PathBuf::from(socket),
        None => match std::env::var("WAYLAND_DISPLAY") {
            Ok(display) => socket_path(&display),
            // From a console there is no Wayland session in the environment:
            // use the running Tileception's socket if there is exactly one.
            Err(_) => only_socket()?,
        },
    };
    let mut stream = UnixStream::connect(&path)?;
    stream.write_all(format!("{}\n", args.join(" ")).as_bytes())?;
    let mut reply = String::new();
    stream.read_to_string(&mut reply)?;
    print!("{reply}");
    if reply.starts_with("error") {
        std::process::exit(1);
    }
    Ok(())
}
