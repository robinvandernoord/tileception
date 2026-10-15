//! A line-based command socket, the counterpart of TrellisWM's `trelliswm://`
//! links. `tileception msg <command>` sends one command and prints the reply.

use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::PathBuf,
    time::Duration,
};

use smithay::reexports::calloop::{generic::Generic, EventLoop, Interest, Mode, PostAction};
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
        .map_err(|e| std::io::Error::other(e.to_string()))?;
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
        world.focus().map(|w| w.0.to_string()).unwrap_or("none".into())
    );
    out.push_str(&format!(
        "locked {} display-asleep {} selected {:?}\n",
        state.is_locked(),
        state.idle.off,
        state.selection.windows.iter().map(|id| id.0).collect::<Vec<_>>()
    ));
    fn walk(state: &Tileception, id: NodeId, depth: usize, out: &mut String) {
        let world = &state.world;
        let tree = world.tree();
        let pad = "  ".repeat(depth);
        let rect = world
            .node_rect(id, world.target_view())
            .map(|r| format!(" @ {:.0},{:.0} {:.0}x{:.0}", r.x, r.y, r.w, r.h))
            .unwrap_or_else(|| " hidden".into());
        let line = match tree.node(id) {
            Node::Split { axis, weights, .. } => format!("split {} {:?} {:.2?}", id.0, axis, weights),
            Node::Tile { views, selected } => {
                let titles: Vec<String> = views
                    .iter()
                    .map(|v| {
                        let app = state.windows.get(v).map(crate::state::app_id).unwrap_or_default();
                        format!("{}:{}", v.0, app)
                    })
                    .collect();
                format!("tile {} [{}] selected {}", id.0, titles.join(", "), selected)
            }
            Node::Desktop { floats, shown } => {
                let f: Vec<String> = floats.iter().map(|w| w.0.to_string()).collect();
                format!("desktop {} shown {} floats [{}]", id.0, shown, f.join(", "))
            }
        };
        out.push_str(&format!("{pad}{line}{rect}\n"));
        for c in tree.children(id) {
            walk(state, *c, depth + 1, out);
        }
    }
    walk(state, tree.root(), 0, &mut out);
    out
}

fn only_socket() -> std::io::Result<PathBuf> {
    let dir = socket_path("x").parent().map(PathBuf::from).unwrap_or_default();
    let found: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("tileception-")))
        .filter(|p| p.extension().is_some_and(|e| e == "sock"))
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
        Some(p) => PathBuf::from(p),
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
