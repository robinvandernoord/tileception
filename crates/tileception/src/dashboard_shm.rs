//! How much shared memory a dashboard plugin may hand Tileception.
//!
//! Every `wl_shm_pool` a client makes is mapped into Tileception, and stays
//! mapped until the pool object and all of its buffers are gone. The 4096
//! pixel cap on a buffer's side says nothing about pools: one may be far
//! larger than any buffer, need never be drawn from, and each costs a
//! mapping of which a process has a limited number. So a dashboard client is
//! also held to a largest pool and to a number of pools and of buffers; one
//! that goes over ends its connection.
//!
//! Smithay owns the pools and tells neither their sizes nor which buffer
//! lies in which pool, and a request does not name the object it makes. What
//! can be counted exactly is counted: the size asked for, the pool objects a
//! client has and the buffers it has. A mapping outlives its pool object
//! only while a buffer lives, so together these bound what a client has
//! mapped at ([`POOLS`] + [`BUFFERS`]) × [`POOL_BYTES`].
//!
//! `wl_shm`, `wl_shm_pool` and shm `wl_buffer` requests pass through here on
//! their way to Smithay. The dashboard's clients are counted; everyone
//! else's requests go straight on, but for a pool resized to nothing, which
//! Smithay would panic on.
//!
//! The buffers a dashboard client passes as dmabufs count as buffers too,
//! in `dmabuf_imported`.

use std::sync::{Arc, Mutex};

use smithay::{
    reexports::wayland_server::{
        Client, DataInit, Dispatch, DisplayHandle, Resource,
        backend::{ClientId, Handle},
        delegate_global_dispatch,
        protocol::{
            wl_buffer::{self, WlBuffer},
            wl_shm::{self, WlShm},
            wl_shm_pool::{self, WlShmPool},
        },
    },
    wayland::shm::{ShmBufferUserData, ShmPoolUserData, ShmState},
};

use crate::{Tileception, state::ClientState};

/// The largest pool, in bytes: one buffer of the largest size an item may
/// commit, 4096 by 4096 pixels of four bytes.
pub const POOL_BYTES: usize = 4096 * 4096 * 4;
/// The pool objects a client may have at once. A plugin has at most six
/// items; one pool for each buffer of each, three buffers deep, is 18.
pub const POOLS: usize = 24;
/// The buffers a client may have at once, shared memory and dmabuf
/// together: the same six items three buffers deep, with as many again for
/// subsurfaces and for the old buffers that linger while an item changes
/// size.
pub const BUFFERS: usize = 48;

/// What a client may not have more of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Over {
    /// A pool larger than [`POOL_BYTES`].
    PoolBytes,
    /// More than [`POOLS`] pool objects.
    Pools,
    /// More than [`BUFFERS`] buffers.
    Buffers,
}

impl Over {
    fn message(self) -> String {
        match self {
            Self::PoolBytes => {
                format!("a dashboard plugin's shared memory pool is at most {} MiB", POOL_BYTES / (1024 * 1024))
            }
            Self::Pools => format!("a dashboard plugin has at most {POOLS} shared memory pools"),
            Self::Buffers => format!("a dashboard plugin has at most {BUFFERS} shared memory buffers"),
        }
    }
}

/// The shared memory objects one client has.
#[derive(Debug, Default)]
pub struct Shared {
    pools: usize,
    buffers: usize,
}

impl Shared {
    /// A new pool of `size` bytes, counted when it is allowed.
    pub fn pool(&mut self, size: usize) -> Result<(), Over> {
        if size > POOL_BYTES {
            return Err(Over::PoolBytes);
        }
        if self.pools >= POOLS {
            return Err(Over::Pools);
        }
        self.pools += 1;
        Ok(())
    }

    /// A pool grows to `size` bytes.
    pub fn resize(&self, size: usize) -> Result<(), Over> {
        if size > POOL_BYTES { Err(Over::PoolBytes) } else { Ok(()) }
    }

    /// A new buffer in a pool, counted when it is allowed.
    pub fn buffer(&mut self) -> Result<(), Over> {
        if self.buffers >= BUFFERS {
            return Err(Over::Buffers);
        }
        self.buffers += 1;
        Ok(())
    }

    pub fn pool_destroyed(&mut self) {
        self.pools = self.pools.saturating_sub(1);
    }

    pub fn buffer_destroyed(&mut self) {
        self.buffers = self.buffers.saturating_sub(1);
    }
}

/// What is kept with a dashboard client's connection.
#[derive(Debug, Default)]
pub struct Counted {
    pub shared: Shared,
    /// The client's `wl_shm`, whose interface has the error codes: a pool
    /// has none of its own.
    shm: Option<WlShm>,
}

/// A dashboard client's count, kept with its connection.
pub type ClientShared = Arc<Mutex<Counted>>;

/// The count of the client, when it is a dashboard plugin's.
pub fn shared_of(client: &Client) -> Option<ClientShared> {
    client.get_data::<ClientState>()?.dashboard.as_ref().map(|owner| owner.shared.clone())
}

/// The count of the client a destroyed object belonged to, when that was a
/// dashboard plugin's. The client is looked up on the display the object
/// itself was on: a client id means nothing on another display, where it
/// could name someone else.
fn shared_of_destroyed(resource: &impl Resource, client: ClientId) -> Option<ClientShared> {
    shared_on(&resource.handle().upgrade()?, client)
}

/// The count of the dashboard plugin a buffer, perhaps destroyed, belonged
/// to, asked of the buffer's own display.
pub fn shared_of_buffer(buffer: &WlBuffer) -> Option<ClientShared> {
    let handle = buffer.handle().upgrade()?;
    let client = handle.get_client(buffer.id()).ok()?;
    shared_on(&handle, client)
}

/// The count of the client with this id on the display `handle` belongs to.
fn shared_on(handle: &Handle, client: ClientId) -> Option<ClientShared> {
    let data = handle.get_client_data(client).ok()?;
    data.downcast_ref::<ClientState>()?.dashboard.as_ref().map(|owner| owner.shared.clone())
}

/// Counts what the client asks for, or ends its connection with the reason.
/// True when the request may go on to Smithay.
fn allowed(client: &Client, resource: &impl Resource, count: impl FnOnce(&mut Shared) -> Result<(), Over>) -> bool {
    let Some(shared) = shared_of(client) else { return true };
    let Ok(mut counted) = shared.lock() else { return true };
    let Err(over) = count(&mut counted.shared) else { return true };
    let plugin = client
        .get_data::<ClientState>()
        .and_then(|data| data.dashboard.as_ref().map(|owner| owner.plugin.clone()))
        .unwrap_or_default();
    tracing::warn!("Plugin {plugin}: {}; its dashboard connection is closed", over.message());
    // wl_shm has no error for a limit; the message says which it was. Sent
    // on the client's wl_shm where it still has one, since the code is that
    // interface's; otherwise on the pool, as Smithay does with its own.
    match counted.shm.as_ref().filter(|shm| shm.is_alive()) {
        Some(shm) => shm.post_error(wl_shm::Error::InvalidFd, over.message()),
        None => resource.post_error(wl_shm::Error::InvalidFd, over.message()),
    }
    false
}

delegate_global_dispatch!(Tileception: [WlShm: ()] => ShmState);

impl Dispatch<WlShm, ()> for Tileception {
    fn request(
        state: &mut Self,
        client: &Client,
        shm: &WlShm,
        request: wl_shm::Request,
        data: &(),
        handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        if let Some(shared) = shared_of(client)
            && let Ok(mut counted) = shared.lock()
        {
            counted.shm = Some(shm.clone());
        }
        // Smithay refuses a size of zero or less itself.
        if let wl_shm::Request::CreatePool { size, .. } = &request
            && let Ok(size) = usize::try_from(*size)
            && size > 0
            && !allowed(client, shm, |shared| shared.pool(size))
        {
            return;
        }
        <ShmState as Dispatch<WlShm, (), Self>>::request(state, client, shm, request, data, handle, data_init);
    }
}

impl Dispatch<WlShmPool, ShmPoolUserData> for Tileception {
    fn request(
        state: &mut Self,
        client: &Client,
        pool: &WlShmPool,
        request: wl_shm_pool::Request,
        data: &ShmPoolUserData,
        handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        let goes_on = match &request {
            wl_shm_pool::Request::CreateBuffer { .. } => allowed(client, pool, Shared::buffer),
            wl_shm_pool::Request::Resize { size } if *size <= 0 => {
                // From any client: Smithay 0.7 posts this error and then
                // goes on to unwrap a size of zero, which panics.
                pool.post_error(wl_shm::Error::InvalidFd, "invalid wl_shm_pool size");
                false
            }
            wl_shm_pool::Request::Resize { size } => {
                let size = usize::try_from(*size).unwrap_or(usize::MAX);
                allowed(client, pool, |shared| shared.resize(size))
            }
            _ => true,
        };
        if goes_on {
            <ShmState as Dispatch<WlShmPool, ShmPoolUserData, Self>>::request(
                state, client, pool, request, data, handle, data_init,
            );
        }
    }

    fn destroyed(state: &mut Self, client: ClientId, pool: &WlShmPool, data: &ShmPoolUserData) {
        if let Some(shared) = shared_of_destroyed(pool, client.clone())
            && let Ok(mut counted) = shared.lock()
        {
            counted.shared.pool_destroyed();
        }
        <ShmState as Dispatch<WlShmPool, ShmPoolUserData, Self>>::destroyed(state, client, pool, data);
    }
}

impl Dispatch<WlBuffer, ShmBufferUserData> for Tileception {
    fn request(
        state: &mut Self,
        client: &Client,
        buffer: &WlBuffer,
        request: wl_buffer::Request,
        data: &ShmBufferUserData,
        handle: &DisplayHandle,
        data_init: &mut DataInit<'_, Self>,
    ) {
        <ShmState as Dispatch<WlBuffer, ShmBufferUserData, Self>>::request(
            state, client, buffer, request, data, handle, data_init,
        );
    }

    fn destroyed(state: &mut Self, client: ClientId, buffer: &WlBuffer, data: &ShmBufferUserData) {
        if let Some(shared) = shared_of_destroyed(buffer, client.clone())
            && let Ok(mut counted) = shared.lock()
        {
            counted.shared.buffer_destroyed();
        }
        <ShmState as Dispatch<WlBuffer, ShmBufferUserData, Self>>::destroyed(state, client, buffer, data);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test setup and assertions should fail immediately on unexpected errors")]
mod tests {
    use super::*;

    #[test]
    fn a_pool_holds_one_largest_buffer_and_no_more() {
        let mut shared = Shared::default();
        assert_eq!(shared.pool(POOL_BYTES), Ok(()));
        assert_eq!(shared.pool(POOL_BYTES + 1), Err(Over::PoolBytes));
        assert_eq!(shared.resize(POOL_BYTES), Ok(()));
        assert_eq!(shared.resize(POOL_BYTES + 1), Err(Over::PoolBytes));
        // The refused pool was not counted.
        assert_eq!(shared.pools, 1);
    }

    #[test]
    fn pools_are_counted_until_they_are_destroyed() {
        let mut shared = Shared::default();
        for _ in 0..POOLS {
            assert_eq!(shared.pool(4096), Ok(()));
        }
        assert_eq!(shared.pool(4096), Err(Over::Pools));
        shared.pool_destroyed();
        assert_eq!(shared.pool(4096), Ok(()));
        assert_eq!(shared.pool(4096), Err(Over::Pools));
    }

    #[test]
    fn buffers_are_counted_apart_from_their_pools() {
        let mut shared = Shared::default();
        // One pool per buffer, destroyed at once as most toolkits do: the
        // buffers keep the memory mapped, so they are what is counted.
        for _ in 0..BUFFERS {
            assert_eq!(shared.pool(4096), Ok(()));
            assert_eq!(shared.buffer(), Ok(()));
            shared.pool_destroyed();
        }
        assert_eq!(shared.pools, 0);
        assert_eq!(shared.buffer(), Err(Over::Buffers));
        shared.buffer_destroyed();
        assert_eq!(shared.buffer(), Ok(()));
    }

    #[test]
    fn a_client_is_found_only_on_its_own_display() {
        use smithay::reexports::wayland_server::Display;

        use crate::dashboard_surface::Owner;

        // Two displays with one client each, the first on each: their ids
        // may be equal, since each display numbers its own clients.
        let main: Display<()> = Display::new().unwrap();
        let dashboard: Display<()> = Display::new().unwrap();
        let (main_end, _main_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let (dashboard_end, _dashboard_peer) = std::os::unix::net::UnixStream::pair().unwrap();
        let shared = ClientShared::default();
        let owner = Owner { plugin: "lamp".into(), shared: shared.clone() };
        let main_client = main.handle().insert_client(main_end, Arc::new(ClientState::default())).unwrap().id();
        let dashboard_client = dashboard
            .handle()
            .insert_client(dashboard_end, Arc::new(ClientState { dashboard: Some(owner), ..ClientState::default() }))
            .unwrap()
            .id();
        let main_handle = main.handle().backend_handle().clone();
        let dashboard_handle = dashboard.handle().backend_handle().clone();
        // Looked up on the wrong display, the main display's client would
        // be taken for the plugin, and each buffer it destroys would come
        // off the plugin's count.
        assert_eq!(main_client, dashboard_client);
        assert!(shared_on(&dashboard_handle, main_client.clone()).is_some());
        // On its own display it is nobody's dashboard plugin.
        assert!(shared_on(&main_handle, main_client).is_none());
        let found = shared_on(&dashboard_handle, dashboard_client).unwrap();
        assert!(Arc::ptr_eq(&found, &shared));
    }

    #[test]
    fn destroying_more_than_was_counted_stays_at_zero() {
        let mut shared = Shared::default();
        shared.pool_destroyed();
        shared.buffer_destroyed();
        assert_eq!((shared.pools, shared.buffers), (0, 0));
    }
}
