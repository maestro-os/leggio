// TODO handle unwraps

use smithay::backend::input::{InputEvent, KeyboardKeyEvent};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::element::surface::{
    WaylandSurfaceRenderElement, render_elements_from_surface_tree,
};
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit;
use smithay::backend::winit::WinitEvent;
use smithay::input::keyboard::FilterResult;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::protocol::wl_buffer::WlBuffer;
use smithay::reexports::wayland_server::protocol::wl_seat::WlSeat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::reexports::winit::platform::pump_events::PumpStatus;
use smithay::utils::{Rectangle, Serial, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    CompositorClientState, CompositorHandler, CompositorState, SurfaceAttributes, TraversalAction,
    with_surface_tree_downward,
};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use std::sync::Arc;
use std::time::Instant;
use tracing::info;
use tracing_subscriber::EnvFilter;

struct Server {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    seat: Seat<Self>,
    seat_state: SeatState<Self>,
    shm_state: ShmState,
}

impl SeatHandler for Server {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
}

impl ShmHandler for Server {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl BufferHandler for Server {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl CompositorHandler for Server {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        on_commit_buffer_handler::<Self>(surface);
    }
}

impl XdgShellHandler for Server {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        info!("new toplevel");
        surface.with_pending_state(|state| {
            state.states.set(xdg_toplevel::State::Activated);
        });
        surface.send_configure();
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

smithay::delegate_compositor!(Server);
smithay::delegate_shm!(Server);
smithay::delegate_seat!(Server);
smithay::delegate_xdg_shell!(Server);

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}

impl ClientData for ClientState {
    fn initialized(&self, _client_id: ClientId) {
        println!("initialized");
    }

    fn disconnected(&self, _client_id: ClientId, _reason: DisconnectReason) {
        println!("disconnected");
    }
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    info!("starting Leggio");
    let mut display: Display<Server> = Display::new().unwrap();
    let display_handle = display.handle();
    let compositor_state = CompositorState::new::<Server>(&display_handle);
    let xdg_shell_state = XdgShellState::new::<Server>(&display_handle);
    let shm_state = ShmState::new::<Server>(&display_handle, vec![]);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&display_handle, "winit");
    let mut state = Server {
        compositor_state,
        xdg_shell_state,
        seat,
        seat_state,
        shm_state,
    };
    let listener = ListeningSocket::bind("wayland-5").unwrap();
    let mut clients = Vec::new();
    let (mut backend, mut winit) = winit::init::<GlesRenderer>().unwrap();
    let start_time = Instant::now();
    let keyboard = state
        .seat
        .add_keyboard(Default::default(), 200, 200)
        .unwrap();

    loop {
        let status = winit.dispatch_new_events(|event| match event {
            WinitEvent::Resized { .. } => {}
            WinitEvent::Input(event) => match event {
                InputEvent::Keyboard { event } => {
                    keyboard.input::<(), _>(
                        &mut state,
                        event.key_code(),
                        event.state(),
                        0.into(),
                        0,
                        |_, _, _| FilterResult::Forward,
                    );
                }
                InputEvent::PointerMotionAbsolute { .. } => {
                    let toplevel = state
                        .xdg_shell_state
                        .toplevel_surfaces()
                        .iter()
                        .next()
                        .cloned();
                    if let Some(surface) = toplevel {
                        let surface = surface.wl_surface().clone();
                        keyboard.set_focus(&mut state, Some(surface), 0.into());
                    }
                }
                _ => {}
            },
            _ => (),
        });

        match status {
            PumpStatus::Continue => (),
            PumpStatus::Exit(_) => break,
        };

        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind().unwrap();
            let elements = state
                .xdg_shell_state
                .toplevel_surfaces()
                .iter()
                .flat_map(|surface| {
                    render_elements_from_surface_tree(
                        renderer,
                        surface.wl_surface(),
                        (0, 0),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    )
                })
                .collect::<Vec<WaylandSurfaceRenderElement<GlesRenderer>>>();

            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Flipped180)
                .unwrap();
            frame
                .clear(Color32F::new(1.0, 0.0, 0.0, 1.0), &[damage])
                .unwrap();
            draw_render_elements(&mut frame, 1.0, &elements, &[damage]).unwrap();
            let _ = frame.finish().unwrap();

            let elapsed = start_time.elapsed().as_millis() as u32;
            for surface in state.xdg_shell_state.toplevel_surfaces() {
                send_frames_surface_tree(surface.wl_surface(), elapsed);
            }

            if let Some(stream) = listener.accept().unwrap() {
                info!("new client"); // TODO add details
                let client = display
                    .handle()
                    .insert_client(stream, Arc::new(ClientState::default()))
                    .unwrap();
                clients.push(client);
            }

            display.dispatch_clients(&mut state).unwrap();
            display.flush_clients().unwrap();
        }
        backend.submit(Some(&[damage])).unwrap();
    }
}

fn send_frames_surface_tree(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_surf, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}
