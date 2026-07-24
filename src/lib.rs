//! Fastmail JMAP client, GraphQL layer, and MCP server.
//!
//! This crate is both a CLI binary (`main.rs`) and a library. The library
//! surface exists so a hosted, multi-tenant MCP service can link the JMAP and
//! GraphQL machinery directly rather than shelling out to the binary.

pub mod carddav;
pub mod commands;
pub mod config;
pub mod error;
pub mod jmap;
pub mod mcp;
pub mod models;
pub mod util;
