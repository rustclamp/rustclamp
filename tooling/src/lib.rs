//! Stable JSON and human queries over Kernel's resolved process projection.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

use rustclamp_core::ApplicationId;
use rustclamp_kernel::{
    ExclusionReason, InclusionReason, ProcessProjection, ProjectionError, ProviderSelection,
};
use serde_json::{Value, json};

/// Current inspection document schema version.
pub const SCHEMA_VERSION: u64 = 1;

/// Serializes one or more projections from the same application.
///
/// The model contains stable semantic identities and resolved metadata only.
/// Configuration values and runtime state are never included.
pub fn inspection_document(projections: &[&ProcessProjection]) -> Result<Value, String> {
    let Some(first) = projections.first() else {
        return Err("at least one process projection is required".into());
    };
    let application = first.application();
    if projections
        .iter()
        .any(|projection| projection.application() != application)
    {
        return Err("all process projections must belong to one application".into());
    }
    let mut processes = projections
        .iter()
        .map(|projection| process_value(projection))
        .collect::<Vec<_>>();
    processes.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));
    Ok(json!({
        "schema_version": SCHEMA_VERSION,
        "status": "resolved",
        "application": application.as_str(),
        "processes": processes,
    }))
}

/// Serializes a projection failure as a machine-readable diagnostic document.
pub fn diagnostic_document(
    application: ApplicationId,
    process: &str,
    error: &ProjectionError,
) -> Value {
    let (code, context) = diagnostic(error);
    json!({
        "schema_version": SCHEMA_VERSION,
        "status": "invalid",
        "application": application.as_str(),
        "process": process,
        "diagnostics": [{
            "code": code,
            "severity": "error",
            "message": diagnostic_message(code, &context),
            "context": context,
        }],
    })
}

/// Validates the required version and top-level document shape.
pub fn validate_document(document: &Value) -> Result<(), String> {
    if document["schema_version"].as_u64() != Some(SCHEMA_VERSION) {
        return Err(format!(
            "unsupported inspection schema version: {}",
            document["schema_version"]
        ));
    }
    if document["status"] == "resolved" && document["processes"].is_array() {
        return Ok(());
    }
    if document["status"] == "invalid" && document["diagnostics"].is_array() {
        return Ok(());
    }
    Err("invalid inspection document shape".into())
}

/// Formats the selected process as a concise human-readable summary.
pub fn inspect_text(process: &Value) -> String {
    format!(
        "Application: {}\nProcess: {}\nRoots: {}\nIncluded modules: {}\nRequirements: {}\nContributions: {}\nExcluded modules: {}",
        process["application"].as_str().unwrap_or("?"),
        process["id"].as_str().unwrap_or("?"),
        join_ids(&process["roots"]),
        count(&process["included_modules"]),
        count(&process["requirements"]),
        count(&process["contributions"]),
        count(&process["exclusions"]),
    )
}

/// Formats the process's resolved inclusion paths as a tree.
pub fn tree_text(process: &Value) -> String {
    let mut lines = vec![format!(
        "{} / {}",
        process["application"].as_str().unwrap_or("?"),
        process["id"].as_str().unwrap_or("?")
    )];
    let mut modules = process["included_modules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    modules.sort_by_key(|module| {
        module["path"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    for module in &modules {
        let path = module["path"].as_array().cloned().unwrap_or_default();
        let depth = path.len().saturating_sub(1);
        lines.push(format!(
            "{}{}",
            "  ".repeat(depth),
            module["id"].as_str().unwrap_or("?")
        ));
    }
    lines.join("\n")
}

/// Formats capability and contribution relationships as directed edges.
pub fn graph_text(process: &Value) -> String {
    let mut edges = Vec::new();
    for module in process["included_modules"].as_array().into_iter().flatten() {
        let path = module["path"].as_array().cloned().unwrap_or_default();
        for pair in path.windows(2) {
            edges.push(format!(
                "{} -> {} [inclusion]",
                display_id(&pair[0]),
                display_id(&pair[1])
            ));
        }
    }
    for requirement in process["requirements"].as_array().into_iter().flatten() {
        if let Some(provider) = requirement["provider"].as_str() {
            edges.push(format!(
                "{} -> {} [capability:{}]",
                display_id(&requirement["consumer"]),
                provider,
                display_id(&requirement["capability"])
            ));
        }
    }
    for contribution in process["contributions"].as_array().into_iter().flatten() {
        edges.push(format!(
            "{} -> {} [contribution:{}]",
            display_id(&contribution["contributor"]),
            display_id(&contribution["consumer"]),
            display_id(&contribution["target"])
        ));
    }
    edges.sort();
    edges.dedup();
    edges.join("\n")
}

/// Explains why a module is included or excluded from a process.
pub fn why_text(process: &Value, module_id: &str) -> Result<String, String> {
    if let Some(module) = process["included_modules"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|module| module["id"].as_str() == Some(module_id))
    {
        let path = module["path"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect::<Vec<_>>()
            .join(" -> ");
        return Ok(format!(
            "{module_id} is included via {path}\nReason: {}",
            reason_text(&module["reason"])
        ));
    }
    if let Some(exclusion) = process["exclusions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|entry| entry["id"].as_str() == Some(module_id))
    {
        return Ok(format!("{module_id} is excluded: {}", exclusion["reason"]));
    }
    Err(format!(
        "module {module_id:?} is not declared in this process"
    ))
}

fn reason_text(reason: &Value) -> String {
    match reason["kind"].as_str() {
        Some("execution_root") => format!(
            "execution root for {}",
            reason["execution"].as_str().unwrap_or("the process")
        ),
        Some("capability_provider") => {
            let selection = reason["selection"].as_str().unwrap_or("selected");
            let mut text = format!(
                "provides {} to {} ({selection} provider)",
                display_id(&reason["capability"]),
                display_id(&reason["required_by"])
            );
            if !reason["replaced_provider"].is_null() {
                text.push_str(&format!(
                    "; replaces {}",
                    display_id(&reason["replaced_provider"])
                ));
            }
            text
        }
        Some("contribution") => format!(
            "contributes {} to {} for target {}",
            display_id(&reason["contribution"]),
            display_id(&reason["consumed_by"]),
            display_id(&reason["target"])
        ),
        _ => "included by the resolved process composition".into(),
    }
}

fn display_id(value: &Value) -> &str {
    value.as_str().unwrap_or("?")
}

/// Formats doctor diagnostics and reports whether the composition is valid.
pub fn doctor_text(document: &Value) -> (String, bool) {
    if document["status"] == "resolved" {
        return ("No composition errors.".into(), true);
    }
    let messages = document["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|diagnostic| {
            format!(
                "{}: {}",
                diagnostic["code"].as_str().unwrap_or("unknown"),
                diagnostic["message"]
                    .as_str()
                    .unwrap_or("composition failed")
            )
        })
        .collect::<Vec<_>>();
    (messages.join("\n"), false)
}

fn process_value(projection: &ProcessProjection) -> Value {
    let mut included = projection
        .included_modules()
        .iter()
        .map(|entry| {
            json!({
                "id": entry.module().as_str(),
                "path": entry.path().iter().map(|id| id.as_str()).collect::<Vec<_>>(),
                "reason": inclusion_reason(entry.reason()),
            })
        })
        .collect::<Vec<_>>();
    included.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));

    let mut exclusions = projection
        .exclusions()
        .iter()
        .map(|entry| {
            json!({
                "id": entry.module().as_str(),
                "reason": match entry.reason() {
                    ExclusionReason::Explicit => "explicit",
                    ExclusionReason::Unreachable => "unreachable",
                },
            })
        })
        .collect::<Vec<_>>();
    exclusions.sort_by(|left, right| left["id"].as_str().cmp(&right["id"].as_str()));

    let mut requirements = projection
        .requirements()
        .iter()
        .map(|entry| {
            json!({
                "consumer": entry.consumer().as_str(),
                "capability": entry.capability().as_str(),
                "qualifier": entry.qualifier().map(|id| id.as_str()),
                "provider": entry.provider().map(|id| id.as_str()),
                "optional": entry.optional(),
                "selection": selection(entry.selection()),
                "replaced_provider": entry.replaced_provider().map(|id| id.as_str()),
            })
        })
        .collect::<Vec<_>>();
    requirements.sort_by_key(|entry| {
        (
            entry["consumer"].as_str().unwrap_or_default().to_owned(),
            entry["capability"].as_str().unwrap_or_default().to_owned(),
            entry["qualifier"].as_str().unwrap_or_default().to_owned(),
        )
    });

    let mut contributions = projection
        .contributions()
        .iter()
        .map(|entry| {
            json!({
                "consumer": entry.consumer().as_str(),
                "contributor": entry.contributor().as_str(),
                "target": entry.target().as_str(),
                "qualifier": entry.qualifier().as_str(),
                "contribution": entry.contribution().as_str(),
            })
        })
        .collect::<Vec<_>>();
    contributions.sort_by_key(|entry| {
        (
            entry["consumer"].as_str().unwrap_or_default().to_owned(),
            entry["target"].as_str().unwrap_or_default().to_owned(),
            entry["contribution"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
        )
    });

    json!({
        "id": projection.process().as_str(),
        "application": projection.application().as_str(),
        "roots": projection.roots().iter().map(|id| id.as_str()).collect::<Vec<_>>(),
        "included_modules": included,
        "exclusions": exclusions,
        "requirements": requirements,
        "contributions": contributions,
        "structural_cost": {
            "included_modules": included.len(),
            "requirements": requirements.len(),
            "contributions": contributions.len(),
            "units": "counts",
        },
    })
}

fn inclusion_reason(reason: InclusionReason) -> Value {
    match reason {
        InclusionReason::ExecutionRoot { execution } => json!({
            "kind": "execution_root",
            "execution": execution.as_str(),
        }),
        InclusionReason::CapabilityProvider {
            required_by,
            capability,
            qualifier,
            selection: selected,
            replaced,
        } => json!({
            "kind": "capability_provider",
            "required_by": required_by.as_str(),
            "capability": capability.as_str(),
            "qualifier": qualifier.map(|id| id.as_str()),
            "selection": selection(selected),
            "replaced_provider": replaced.map(|id| id.as_str()),
        }),
        InclusionReason::Contribution {
            consumed_by,
            target,
            qualifier,
            contribution,
        } => json!({
            "kind": "contribution",
            "consumed_by": consumed_by.as_str(),
            "target": target.as_str(),
            "qualifier": qualifier.as_str(),
            "contribution": contribution.as_str(),
        }),
    }
}

fn selection(selection: ProviderSelection) -> &'static str {
    match selection {
        ProviderSelection::Unique => "unique",
        ProviderSelection::Default => "default",
        ProviderSelection::Explicit => "explicit",
        ProviderSelection::OptionalAbsent => "optional_absent",
    }
}

fn diagnostic(error: &ProjectionError) -> (&'static str, Value) {
    match error {
        ProjectionError::MissingProcess {
            application,
            process,
        } => (
            "composition.process.missing",
            json!({"application": application.as_str(), "process": process.as_str()}),
        ),
        ProjectionError::EmptyProcessRoots {
            application,
            process,
        } => (
            "composition.process.empty_roots",
            json!({"application": application.as_str(), "process": process.as_str()}),
        ),
        ProjectionError::MissingExecution {
            application,
            process,
            execution,
        } => (
            "composition.execution.missing",
            json!({"application": application.as_str(), "process": process.as_str(), "execution": execution.as_str()}),
        ),
        ProjectionError::MissingModule {
            application,
            process,
            module,
        } => (
            "composition.module.missing",
            json!({"application": application.as_str(), "process": process.as_str(), "module": module.as_str()}),
        ),
        ProjectionError::MissingProvider {
            application,
            process,
            required_by,
            capability,
            qualifier,
        } => (
            "composition.provider.missing",
            json!({"application": application.as_str(), "process": process.as_str(), "required_by": required_by.as_str(), "capability": capability.as_str(), "qualifier": qualifier.map(|id| id.as_str())}),
        ),
        ProjectionError::AmbiguousProvider {
            application,
            process,
            required_by,
            capability,
            qualifier,
            candidates,
        } => (
            "composition.provider.ambiguous",
            json!({"application": application.as_str(), "process": process.as_str(), "required_by": required_by.as_str(), "capability": capability.as_str(), "qualifier": qualifier.map(|id| id.as_str()), "candidates": candidates.iter().map(|id| id.as_str()).collect::<Vec<_>>()}),
        ),
        ProjectionError::UnavailableProvider {
            application,
            process,
            required_by,
            capability,
            qualifier,
            provider,
        } => (
            "composition.provider.unavailable",
            json!({"application": application.as_str(), "process": process.as_str(), "required_by": required_by.as_str(), "capability": capability.as_str(), "qualifier": qualifier.map(|id| id.as_str()), "provider": provider.as_str()}),
        ),
        ProjectionError::ExcludedProvider {
            application,
            process,
            required_by,
            capability,
            qualifier,
            provider,
        } => (
            "composition.provider.excluded",
            json!({"application": application.as_str(), "process": process.as_str(), "required_by": required_by.as_str(), "capability": capability.as_str(), "qualifier": qualifier.map(|id| id.as_str()), "provider": provider.as_str()}),
        ),
        ProjectionError::ExcludedRoot {
            application,
            process,
            module,
        } => (
            "composition.root.excluded",
            json!({"application": application.as_str(), "process": process.as_str(), "module": module.as_str()}),
        ),
        ProjectionError::DependencyCycle {
            application,
            process,
            path,
        } => (
            "composition.dependency.cycle",
            json!({"application": application.as_str(), "process": process.as_str(), "path": path.iter().map(|id| id.as_str()).collect::<Vec<_>>()}),
        ),
        ProjectionError::OrphanContribution {
            application,
            process,
            contributor,
            target,
            qualifier,
            contribution,
        } => (
            "composition.contribution.orphan",
            json!({"application": application.as_str(), "process": process.as_str(), "contributor": contributor.as_str(), "target": target.as_str(), "qualifier": qualifier.as_str(), "contribution": contribution.as_str()}),
        ),
    }
}

fn diagnostic_message(code: &str, context: &Value) -> String {
    match code {
        "composition.process.missing" => format!(
            "application {} has no process {}; check the process declaration",
            context["application"], context["process"]
        ),
        "composition.process.empty_roots" => format!(
            "process {} has no execution roots; declare at least one root",
            context["process"]
        ),
        "composition.execution.missing" => format!(
            "process {} references missing execution {}; check its execution ID",
            context["process"], context["execution"]
        ),
        "composition.module.missing" => format!(
            "process {} references missing module {}; check its module ID",
            context["process"], context["module"]
        ),
        "composition.provider.missing" => format!(
            "{} requires {}, but no provider is available; add a provider or make the requirement optional",
            context["required_by"], context["capability"]
        ),
        "composition.provider.ambiguous" => format!(
            "{} requires {}, but multiple providers match; select a default or explicit provider",
            context["required_by"], context["capability"]
        ),
        "composition.provider.unavailable" => format!(
            "{} selects unavailable provider {}; include that provider in the process",
            context["required_by"], context["provider"]
        ),
        "composition.provider.excluded" => format!(
            "{} requires {}, but selected provider {} is excluded; remove the exclusion or select another provider",
            context["required_by"], context["capability"], context["provider"]
        ),
        "composition.root.excluded" => format!(
            "execution root {} is excluded; remove it from the exclusion list",
            context["module"]
        ),
        "composition.dependency.cycle" => format!("dependency cycle: {}", context["path"]),
        "composition.contribution.orphan" => format!(
            "contributor {} targets {}, which is not included; include its consumer or remove the contribution",
            context["contributor"], context["target"]
        ),
        _ => format!("composition is invalid ({code})"),
    }
}

fn count(value: &Value) -> usize {
    value.as_array().map_or(0, Vec::len)
}

fn join_ids(value: &Value) -> String {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustclamp_core::{CapabilityId, ExecutionId, ModuleId, ProcessId};
    use rustclamp_kernel::ApplicationBlueprint;

    #[test]
    fn accepts_resolved_and_diagnostic_schema_v1_documents() {
        let resolved = json!({"schema_version": 1, "status": "resolved", "processes": []});
        let invalid = json!({"schema_version": 1, "status": "invalid", "diagnostics": []});
        assert!(validate_document(&resolved).is_ok());
        assert!(validate_document(&invalid).is_ok());
    }

    #[test]
    fn rejects_unknown_schema_versions_and_malformed_documents() {
        assert!(validate_document(&json!({"schema_version": 2})).is_err());
        assert!(validate_document(&json!({"schema_version": 1, "status": "resolved"})).is_err());
    }

    #[test]
    fn explanation_and_tree_use_resolved_paths() {
        let process = json!({
            "application": "sample", "id": "cli",
            "included_modules": [
                {"id": "app", "path": ["app"], "reason": {"kind": "execution_root"}},
                {"id": "db", "path": ["app", "db"], "reason": {"kind": "capability_provider"}}
            ],
            "exclusions": [], "roots": ["app"], "requirements": [], "contributions": []
        });
        assert_eq!(tree_text(&process), "sample / cli\napp\n  db");
        assert!(why_text(&process, "db").unwrap().contains("app -> db"));
        assert!(why_text(&process, "missing").is_err());
    }

    #[test]
    fn graph_formats_semantic_ids_without_json_quotes() {
        let process = json!({
            "included_modules": [
                {"path": ["root", "provider"]}
            ],
            "requirements": [
                {"consumer": "root", "capability": "storage", "provider": "provider"}
            ],
            "contributions": [
                {"contributor": "command", "consumer": "root", "target": "cli"}
            ]
        });
        assert_eq!(
            graph_text(&process),
            "command -> root [contribution:cli]\nroot -> provider [capability:storage]\nroot -> provider [inclusion]"
        );
    }

    #[test]
    fn inspection_document_uses_kernel_resolved_projection() {
        let root = ModuleId::new("app.cli");
        let provider = ModuleId::new("storage.memory");
        let mut blueprint = ApplicationBlueprint::new(ApplicationId::new("sample"));
        blueprint
            .add_module(root)
            .add_module(provider)
            .add_execution(ExecutionId::new("cli"), root)
            .add_process(ProcessId::new("process.cli"), vec![ExecutionId::new("cli")])
            .require_provider(root, CapabilityId::new("storage.database"), None, provider);
        let projection = blueprint.project(ProcessId::new("process.cli")).unwrap();
        let document = inspection_document(&[&projection]).unwrap();
        validate_document(&document).unwrap();
        let process = &document["processes"][0];
        assert_eq!(process["requirements"][0]["provider"], "storage.memory");
        assert!(
            why_text(process, "storage.memory")
                .unwrap()
                .contains("provides storage.database to app.cli")
        );
    }
}
