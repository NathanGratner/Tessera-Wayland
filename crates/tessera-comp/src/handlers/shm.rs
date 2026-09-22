use smithay::{
    delegate_shm,
    wayland::shm::{ShmHandler, ShmState},
};

use crate::state::Tessera;

impl ShmHandler for Tessera {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

delegate_shm!(Tessera);
