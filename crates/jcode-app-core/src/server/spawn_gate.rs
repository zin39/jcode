//! The spawn-path model gate.
//!
//! Split out of comm_session.rs, which is over the code-size ratchet.

/// THE spawn-path gate.
///
/// Whatever resolved the model -- an explicit `model` arg, the
/// `agents.swarm_model` pin, or coordinator inheritance -- a worker must never
/// run on a model excluded by `cheap_route_ban`. Without this a coordinator can
/// spawn its whole swarm onto the frontier model, which is both the expensive
/// failure and the one the user cannot see until billed. Fail loudly instead.
pub(super) fn reject_banned_worker_model(model: Option<&str>) -> anyhow::Result<()> {
    let Some(model) = model else { return Ok(()) };
    if crate::agent::cheap_route::model_is_cheap_route_banned(model) {
        anyhow::bail!(
            "refusing to spawn a worker on '{model}': excluded by agents.cheap_route_ban. \
             Pass an allowed model, or remove it from the ban list."
        );
    }
    Ok(())
}
