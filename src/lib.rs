//! Cœur d'actor-agent : configuration, acteur agent et interface terminal.
//! Le binaire (`main.rs`) ne fait que les assembler ; d'autres frontaux peuvent réutiliser ce cœur.

pub mod agent_actor;
pub mod cli_ui;
pub mod config;
pub mod harness;
#[cfg(test)]
mod test_support;
