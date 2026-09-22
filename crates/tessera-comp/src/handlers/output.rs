use smithay::{delegate_output, wayland::output::OutputHandler};

use crate::state::Tessera;

impl OutputHandler for Tessera {}

delegate_output!(Tessera);
