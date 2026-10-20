//! A client for System One decision models, built from the config file's
//! `decision_model` pair.
//!
//! A decision model generates no text: `POST {base_url}/systemone` takes a
//! `state` plus named, typed questions and answers each with probabilities
//! (`docs/provider-configuration.md`). Approval judgments and
//! turn-continuation checks ask it through [`DecisionClient`].

mod client;
mod config;
mod wire;

pub use client::{DecisionClient, HttpDecisionClient};
pub use config::DecisionConfig;
pub use wire::{Answer, DecisionRequest, DecisionResponse, NoulCriteria, Question, Usage};
