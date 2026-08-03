//! Le convenzioni della famiglia Fedora.

use super::{firewalld::Firewalld, Distro, Firewall};

/// Fedora: firewall `firewalld`.
#[derive(Debug, Default)]
pub struct Fedora {
    firewall: Firewalld,
}

impl Fedora {
    pub const fn new() -> Self {
        Fedora {
            firewall: Firewalld,
        }
    }
}

impl Distro for Fedora {
    fn firewall(&self) -> &dyn Firewall {
        &self.firewall
    }
}
