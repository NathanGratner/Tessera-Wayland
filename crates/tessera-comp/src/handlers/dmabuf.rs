//! `linux-dmabuf`: buffers clients render on the GPU (S7).
//!
//! Only advertised by the real-hardware backend, which owns the renderer that
//! decides whether a buffer can be imported. Nested Tessera has no dmabuf
//! global, so GL clients fall back to shared memory there.

use smithay::{
    backend::allocator::dmabuf::Dmabuf,
    delegate_dmabuf,
    wayland::dmabuf::{DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier},
};

use crate::state::Tessera;

impl DmabufHandler for Tessera {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self
            .udev
            .as_mut()
            .expect("dmabuf is only advertised by the udev backend")
            .dmabuf
            .as_mut()
            .expect("the dmabuf global exists, so its state does")
            .0
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: Dmabuf,
        notifier: ImportNotifier,
    ) {
        let imported = self
            .udev
            .as_mut()
            .is_some_and(|udev| udev.can_import(&dmabuf));
        if imported {
            let _ = notifier.successful::<Tessera>();
        } else {
            notifier.failed();
        }
    }
}

delegate_dmabuf!(Tessera);
