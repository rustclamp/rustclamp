//! Prints process boundaries, target contributions, and resource ownership.

use rustclamp_core::{
    ApplicationId, Capability, CapabilityId, ClockCapability, Contribution, ContributionTarget,
    ExecutionId, ModuleId, ProcessId, QualifierId,
};
use rustclamp_kernel::{ApplicationBlueprint, ProcessProjection};
use rustclamp_messaging::MessageBusCapability;
use rustclamp_scheduler::{JobDeclaration, SchedulerTarget};
use rustclamp_worker::{HandlerDeclaration, HandlerTarget};
use std::error::Error;

const APPLICATION: ApplicationId = ApplicationId::new("example.phase7.create-order");
const API: ProcessId = ProcessId::new("orders-api");
const PUBLISHER: ProcessId = ProcessId::new("outbox-publisher");
const WORKER: ProcessId = ProcessId::new("orders-worker");
const SCHEDULER: ProcessId = ProcessId::new("orders-scheduler");
const API_EXECUTION: ExecutionId = ExecutionId::new("orders-api.main");
const PUBLISHER_EXECUTION: ExecutionId = ExecutionId::new("outbox-publisher.main");
const WORKER_EXECUTION: ExecutionId = ExecutionId::new("orders-worker.main");
const SCHEDULER_EXECUTION: ExecutionId = ExecutionId::new("orders-scheduler.main");
const API_ROOT: ModuleId = ModuleId::new("orders.api");
const PUBLISHER_ROOT: ModuleId = ModuleId::new("orders.outbox-publisher");
const WORKER_ROOT: ModuleId = ModuleId::new("orders.worker");
const SCHEDULER_ROOT: ModuleId = ModuleId::new("orders.scheduler");
const DATABASE: ModuleId = ModuleId::new("orders.postgres-pool");
const JETSTREAM: ModuleId = ModuleId::new("orders.jetstream-client");
const SYSTEM_CLOCK: ModuleId = ModuleId::new("orders.system-clock");
const ORDER_HANDLER: ModuleId = ModuleId::new("orders.fulfillment-handler");
const RECONCILE_JOB: ModuleId = ModuleId::new("orders.payment-reconciliation-job");
const DATABASE_CAPABILITY: CapabilityId = CapabilityId::new("orders.database");
const HANDLERS: QualifierId = QualifierId::new("orders.worker.handlers");
const JOBS: QualifierId = QualifierId::new("orders.scheduler.jobs");

fn architecture() -> ApplicationBlueprint {
    let mut blueprint = ApplicationBlueprint::new(APPLICATION);
    for module in [
        API_ROOT,
        PUBLISHER_ROOT,
        WORKER_ROOT,
        SCHEDULER_ROOT,
        DATABASE,
        JETSTREAM,
        SYSTEM_CLOCK,
        ORDER_HANDLER,
        RECONCILE_JOB,
    ] {
        blueprint.add_module(module);
    }
    blueprint
        .add_execution(API_EXECUTION, API_ROOT)
        .add_execution(PUBLISHER_EXECUTION, PUBLISHER_ROOT)
        .add_execution(WORKER_EXECUTION, WORKER_ROOT)
        .add_execution(SCHEDULER_EXECUTION, SCHEDULER_ROOT)
        .add_process(API, vec![API_EXECUTION])
        .add_process(PUBLISHER, vec![PUBLISHER_EXECUTION])
        .add_process(WORKER, vec![WORKER_EXECUTION])
        .add_process(SCHEDULER, vec![SCHEDULER_EXECUTION])
        .require_provider(API_ROOT, DATABASE_CAPABILITY, None, DATABASE)
        .require_provider(PUBLISHER_ROOT, DATABASE_CAPABILITY, None, DATABASE)
        .require_provider(PUBLISHER_ROOT, MessageBusCapability::ID, None, JETSTREAM)
        .require_provider(WORKER_ROOT, DATABASE_CAPABILITY, None, DATABASE)
        .require_provider(WORKER_ROOT, MessageBusCapability::ID, None, JETSTREAM)
        .require_provider(SCHEDULER_ROOT, ClockCapability::ID, None, SYSTEM_CLOCK)
        .consume_target(WORKER_ROOT, HandlerTarget::ID, HANDLERS)
        .add_contribution(
            ORDER_HANDLER,
            HandlerTarget::ID,
            HANDLERS,
            HandlerDeclaration::ID,
        )
        .consume_target(SCHEDULER_ROOT, SchedulerTarget::ID, JOBS)
        .add_contribution(RECONCILE_JOB, SchedulerTarget::ID, JOBS, JobDeclaration::ID);
    blueprint
}

fn describe(projection: &ProcessProjection, output: &mut String) {
    use std::fmt::Write;
    let _ = writeln!(
        output,
        "process {} roots={:?}",
        projection.process().as_str(),
        projection.roots()
    );
    for included in projection.included_modules() {
        let path = included
            .path()
            .iter()
            .map(|id| id.as_str())
            .collect::<Vec<_>>()
            .join(" -> ");
        let _ = writeln!(output, "  module {} via {path}", included.module().as_str());
    }
    for requirement in projection.requirements() {
        let _ = writeln!(
            output,
            "  ownership {} --{}--> {}",
            requirement.consumer().as_str(),
            requirement.capability().as_str(),
            requirement
                .provider()
                .map(ModuleId::as_str)
                .unwrap_or("unresolved"),
        );
    }
    for contribution in projection.contributions() {
        let _ = writeln!(
            output,
            "  contribution {} --{} [{}]--> {}",
            contribution.contributor().as_str(),
            contribution.contribution().as_str(),
            contribution.qualifier().as_str(),
            contribution.target().as_str(),
        );
    }
}

fn inspection_text() -> Result<String, Box<dyn Error>> {
    let blueprint = architecture();
    let mut output = String::new();
    for process in [API, PUBLISHER, WORKER, SCHEDULER] {
        let projection = blueprint
            .project(process)
            .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
        describe(&projection, &mut output);
    }
    output.push_str(
        "message boundary orders.outbox-publisher --orders.order-created/v1--> JetStream --orders.order-created/v1--> orders.fulfillment-handler\n",
    );
    output.push_str(
        "lifecycle ownership: API owns its managed pool; publisher and worker own separate pools and JetStream clients; each process drains its accepted work on Ctrl-C; scheduler owns the admission gate and active tick futures\n",
    );
    Ok(output)
}

#[cfg(feature = "tooling-inspection")]
fn tooling_document() -> Result<(), Box<dyn Error>> {
    let blueprint = architecture();
    let projections = [API, PUBLISHER, WORKER, SCHEDULER]
        .into_iter()
        .map(|process| {
            blueprint
                .project(process)
                .map_err(|error| std::io::Error::other(format!("{error:?}")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let projections = projections.iter().collect::<Vec<_>>();
    let document =
        rustclamp_tooling::inspection_document(&projections).map_err(std::io::Error::other)?;
    println!("{document}");
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    #[cfg(feature = "tooling-inspection")]
    if std::env::args().nth(1).as_deref() == Some("--tooling-json") {
        return tooling_document();
    }
    print!("{}", inspection_text()?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspection_shows_process_roots_boundaries_contributions_and_ownership() {
        let text = inspection_text().expect("valid architecture");
        for expected in [
            "process orders-api",
            "process outbox-publisher",
            "process orders-worker",
            "process orders-scheduler",
            "orders.order-created/v1",
            "orders.fulfillment-handler",
            "rustclamp.worker.handlers",
            "rustclamp.scheduler.jobs",
            "ownership orders.worker --messaging.bus--> orders.jetstream-client",
            "lifecycle ownership:",
        ] {
            assert!(
                text.contains(expected),
                "missing inspection detail: {expected}"
            );
        }
    }
}
