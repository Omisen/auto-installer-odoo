//! Le convenzioni della famiglia Debian (Debian e Ubuntu).

use super::{ufw::Ufw, Distro, Firewall};

/// Debian/Ubuntu: firewall `ufw`.
#[derive(Debug, Default)]
pub struct Debian {
    firewall: Ufw,
}

impl Debian {
    pub const fn new() -> Self {
        Debian { firewall: Ufw }
    }
}

impl Distro for Debian {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }
}
