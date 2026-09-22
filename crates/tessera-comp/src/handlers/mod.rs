//! Wayland protocol handlers, each wired to `Tessera` with its `delegate_*!` macro.

mod activation;
mod compositor;
mod decoration;
mod dmabuf;
mod output;
mod seat;
mod shm;
mod xdg_shell;
