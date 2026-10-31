//! Library custom properties RPC (Web host port of the desktop Agentero 0.14
//! feature, `d67d4151` / `ba7bddf0`).
//!
//! Definitions live in `.agentero/properties.json` + the catalog
//! `custom_properties` table; per-paper values live in
//! `papers.custom_props_json` (schema v11). All commands take the standard
//! camelCase argument object and answer the plain `ApiResult<T>` envelope.
//!
//! Wire shapes:
//! - `custom_properties_list({vaultPath})` → `{properties: CustomProperty[]}`
//! - `custom_properties_create({vaultPath,name,propertyType?,options?})`
//!   → `CustomProperty`
//! - `custom_properties_update({vaultPath,property})` → `CustomProperty`
//! - `custom_properties_delete({vaultPath,propertyId})` → `null`
//! - `custom_properties_set_paper_value({vaultPath,paperRef,propertyId,value?})`
//!   → `{path,values}`
//! - `custom_properties_create_option({vaultPath,paperRef,propertyId,optionName,color?})`
//!   → `{property,path,values}`
//! - `custom_properties_delete_option({vaultPath,propertyId,optionId})`
//!   → `CustomProperty`
//! - `custom_properties_paper_values({vaultPath})` → `{valuesByPath}`

use agentero_core::features::paper::catalog::papers::{self, PaperRefLookup};
use agentero_core::features::paper::catalog::properties::{self, CustomProperty, PropertyOption};
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::Path;

use crate::error::{blocking, fail_msg, ok, parse_args, vault_root, HandlerResult};
use crate::rpc::RpcCtx;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PropertyOptionArg {
    #[serde(default)]
    id: String,
    name: String,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateArgs {
    name: String,
    #[serde(default)]
    property_type: Option<String>,
    #[serde(default)]
    options: Vec<PropertyOptionArg>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct UpdateArgs {
    property: CustomProperty,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteArgs {
    property_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SetValueArgs {
    /// Paper id or vault-relative folder path (`papers/…`).
    paper_ref: String,
    property_id: String,
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateOptionArgs {
    paper_ref: String,
    property_id: String,
    option_name: String,
    #[serde(default)]
    color: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeleteOptionArgs {
    property_id: String,
    option_id: String,
}

fn to_options(options: Vec<PropertyOptionArg>) -> Vec<PropertyOption> {
    options
        .into_iter()
        .map(|option| PropertyOption {
            id: option.id,
            name: option.name,
            color: option.color,
        })
        .collect()
}

/// Resolve a user-supplied paper ref to its catalog path (rejects unknown or
/// ambiguous refs and never accepts paths outside the catalog).
fn resolve_paper_path(vault: &Path, reference: &str) -> Result<String, String> {
    match papers::lookup_paper_ref(vault, reference.trim()) {
        Ok(PaperRefLookup::Found(record)) => Ok(record.path),
        Ok(PaperRefLookup::NotFound) => Err(format!("paper not found: {reference}")),
        Ok(PaperRefLookup::Ambiguous(paths)) => Err(format!(
            "paper ref '{}' is ambiguous ({} matches): {}",
            reference,
            paths.len(),
            paths.join(", ")
        )),
        Err(error) => Err(error.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Sync command bodies (transport-independent so they are unit-testable).
// ---------------------------------------------------------------------------

fn list_at(vault: &Path) -> Result<Value, String> {
    let properties = properties::list_properties(vault).map_err(|e| e.to_string())?;
    Ok(json!({ "properties": properties }))
}

fn create_at(vault: &Path, args: CreateArgs) -> Result<CustomProperty, String> {
    let property_type = args.property_type.unwrap_or_default();
    properties::create_property(vault, &args.name, &property_type, to_options(args.options))
        .map_err(|e| e.to_string())
}

fn update_at(vault: &Path, args: UpdateArgs) -> Result<CustomProperty, String> {
    properties::update_property(vault, &args.property).map_err(|e| e.to_string())
}

fn delete_at(vault: &Path, args: DeleteArgs) -> Result<Value, String> {
    properties::delete_property(vault, &args.property_id).map_err(|e| e.to_string())?;
    Ok(Value::Null)
}

fn set_paper_value_at(vault: &Path, args: SetValueArgs) -> Result<Value, String> {
    let path = resolve_paper_path(vault, &args.paper_ref)?;
    let values =
        properties::set_paper_property(vault, &path, &args.property_id, args.value.as_deref())
            .map_err(|e| e.to_string())?;
    Ok(json!({ "path": path, "values": values }))
}

fn create_option_at(vault: &Path, args: CreateOptionArgs) -> Result<Value, String> {
    let path = resolve_paper_path(vault, &args.paper_ref)?;
    let (property, values) = properties::create_option_and_set(
        vault,
        &path,
        &args.property_id,
        &args.option_name,
        args.color.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    Ok(json!({ "property": property, "path": path, "values": values }))
}

fn delete_option_at(vault: &Path, args: DeleteOptionArgs) -> Result<CustomProperty, String> {
    properties::delete_property_option(vault, &args.property_id, &args.option_id)
        .map_err(|e| e.to_string())
}

fn paper_values_at(vault: &Path) -> Result<Value, String> {
    let values = properties::paper_values(vault).map_err(|e| e.to_string())?;
    Ok(json!({ "valuesByPath": values }))
}

/// Shared envelope for the plain-`ApiResult` commands: read `vaultPath` from
/// the wire args, run the blocking body, then wrap the value with [`ok`].
async fn run<T, F>(args: Value, body: F) -> HandlerResult
where
    T: serde::Serialize + Send + 'static,
    F: FnOnce(std::path::PathBuf) -> Result<T, String> + Send + 'static,
{
    let vault_path = match args.get("vaultPath").and_then(Value::as_str) {
        Some(path) => path.to_string(),
        None => return fail_msg("invalid_args: missing vaultPath"),
    };
    let result = blocking(move || {
        let vault = vault_root(&vault_path)?;
        body(vault)
    })
    .await;
    match result {
        Ok(value) => ok(value),
        Err(error) => fail_msg(error),
    }
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `custom_properties_list({vaultPath})` → `ApiResult<{properties}>`.
pub async fn list(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    run(args.clone(), |vault| list_at(&vault)).await
}

/// `custom_properties_create(...)` → `ApiResult<CustomProperty>`.
pub async fn create(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let parsed: CreateArgs = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return fail_msg(error),
    };
    run(args.clone(), move |vault| create_at(&vault, parsed)).await
}

/// `custom_properties_update({vaultPath,property})` → `ApiResult<CustomProperty>`.
pub async fn update(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let parsed: UpdateArgs = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return fail_msg(error),
    };
    run(args.clone(), move |vault| update_at(&vault, parsed)).await
}

/// `custom_properties_delete({vaultPath,propertyId})` → `ApiResult<null>`.
pub async fn delete(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let parsed: DeleteArgs = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return fail_msg(error),
    };
    run(args.clone(), move |vault| delete_at(&vault, parsed)).await
}

/// `custom_properties_set_paper_value(...)` → `ApiResult<{path,values}>`.
pub async fn set_paper_value(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let parsed: SetValueArgs = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return fail_msg(error),
    };
    run(args.clone(), move |vault| {
        set_paper_value_at(&vault, parsed)
    })
    .await
}

/// `custom_properties_create_option(...)` → `ApiResult<{property,path,values}>`.
pub async fn create_option(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let parsed: CreateOptionArgs = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return fail_msg(error),
    };
    run(args.clone(), move |vault| create_option_at(&vault, parsed)).await
}

/// `custom_properties_delete_option(...)` → `ApiResult<CustomProperty>`.
pub async fn delete_option(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    let parsed: DeleteOptionArgs = match parse_args(args) {
        Ok(parsed) => parsed,
        Err(error) => return fail_msg(error),
    };
    run(args.clone(), move |vault| delete_option_at(&vault, parsed)).await
}

/// `custom_properties_paper_values({vaultPath})` → `ApiResult<{valuesByPath}>`.
pub async fn paper_values(_ctx: &RpcCtx, args: &Value) -> HandlerResult {
    run(args.clone(), |vault| paper_values_at(&vault)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use agentero_core::features::paper::catalog::papers::{self as catalog, PaperRecord};
    use tempfile::tempdir;

    fn seed_paper(root: &Path, name: &str) {
        let path = format!("papers/{name}");
        std::fs::create_dir_all(root.join(&path)).unwrap();
        catalog::upsert_paper(
            root,
            &PaperRecord::local_pdf(name.to_string(), format!("Paper {name}")).at_path(&path),
        )
        .unwrap();
    }

    fn create_args(name: &str, property_type: &str) -> CreateArgs {
        CreateArgs {
            name: name.to_string(),
            property_type: Some(property_type.to_string()),
            options: vec![PropertyOptionArg {
                id: String::new(),
                name: "Read".into(),
                color: Some("green".into()),
            }],
        }
    }

    #[test]
    fn wire_args_are_camel_case_and_tolerate_optional_fields() {
        let parsed: CreateArgs = parse_args(&json!({
            "vaultPath": "/vault",
            "name": "Status",
            "propertyType": "select",
            "options": [{"name": "Read", "color": "green"}]
        }))
        .expect("camelCase create args");
        assert_eq!(parsed.property_type.as_deref(), Some("select"));
        assert_eq!(parsed.options[0].name, "Read");
        assert_eq!(parsed.options[0].color.as_deref(), Some("green"));

        let parsed: SetValueArgs = parse_args(&json!({
            "vaultPath": "/vault",
            "paperRef": "papers/demo",
            "propertyId": "prop_1",
            "value": null
        }))
        .expect("camelCase set-value args");
        assert_eq!(parsed.paper_ref, "papers/demo");
        assert!(parsed.value.is_none());

        let parsed: UpdateArgs = parse_args(&json!({
            "vaultPath": "/vault",
            "property": {
                "id": "prop_1",
                "name": "Status",
                "type": "select",
                "orderIndex": 0,
                "options": [{"id": "opt_1", "name": "Read"}],
                "createdAt": "t",
                "updatedAt": "t"
            }
        }))
        .expect("camelCase update args");
        assert_eq!(parsed.property.property_type, "select");
        assert_eq!(parsed.property.options[0].id, "opt_1");
    }

    #[test]
    fn commands_round_trip_through_the_catalog() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        seed_paper(root, "alpha");

        // Create + list definitions (definitions also mirror to properties.json).
        let created = create_at(root, create_args("Status", "select")).unwrap();
        assert_eq!(created.name, "Status");
        assert_eq!(created.options.len(), 1);
        assert_eq!(
            list_at(root).unwrap()["properties"]
                .as_array()
                .unwrap()
                .len(),
            1
        );

        // Assign the existing option, then observe the wire value map.
        let update = set_paper_value_at(
            root,
            SetValueArgs {
                paper_ref: "papers/alpha".into(),
                property_id: created.id.clone(),
                value: Some(created.options[0].id.clone()),
            },
        )
        .unwrap();
        assert_eq!(update["path"], "papers/alpha");
        assert_eq!(
            update["values"].get(created.id.as_str()),
            Some(&json!(created.options[0].id))
        );
        let values = paper_values_at(root).unwrap();
        assert_eq!(
            values["valuesByPath"]["papers/alpha"].get(created.id.as_str()),
            Some(&json!(created.options[0].id))
        );

        // Unknown paper refs are rejected by the handler-level resolver.
        let error = set_paper_value_at(
            root,
            SetValueArgs {
                paper_ref: "papers/missing".into(),
                property_id: created.id.clone(),
                value: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("paper not found"), "{error}");

        // Delete removes the definition; values are cleared server-side.
        delete_at(
            root,
            DeleteArgs {
                property_id: created.id,
            },
        )
        .unwrap();
        assert_eq!(
            list_at(root).unwrap()["properties"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn create_option_assigns_it_to_the_paper() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        seed_paper(root, "demo");
        let created = create_at(root, create_args("Status", "select")).unwrap();
        let empty = create_at(root, create_args("Empty", "select")).unwrap();
        let result = create_option_at(
            root,
            CreateOptionArgs {
                paper_ref: "papers/demo".into(),
                property_id: empty.id.clone(),
                option_name: "To read".into(),
                color: Some("blue".into()),
            },
        )
        .unwrap();
        assert_eq!(result["property"]["name"], "Empty");
        let options = result["property"]["options"].as_array().unwrap();
        let new_option = options
            .iter()
            .find(|option| option["name"] == "To read")
            .expect("created option is advertised on the definition");
        assert_eq!(
            result["values"].get(empty.id.as_str()),
            Some(&new_option["id"])
        );
        assert_ne!(created.id, empty.id);
    }

    #[test]
    fn invalid_property_and_option_inputs_surface_business_errors() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        seed_paper(root, "alpha");
        assert!(create_at(root, create_args("  ", "select")).is_err());
        let created = create_at(root, create_args("Status", "select")).unwrap();
        let error = set_paper_value_at(
            root,
            SetValueArgs {
                paper_ref: "papers/alpha".into(),
                property_id: created.id.clone(),
                value: Some("opt_missing".into()),
            },
        )
        .unwrap_err();
        assert!(error.contains("does not exist"), "{error}");
        let empty = create_at(root, create_args("Empty", "select")).unwrap();
        let error = create_option_at(
            root,
            CreateOptionArgs {
                paper_ref: "papers/alpha".into(),
                property_id: empty.id.clone(),
                option_name: " ".into(),
                color: None,
            },
        )
        .unwrap_err();
        assert!(error.contains("option name cannot be empty"), "{error}");
    }

    #[test]
    fn properties_without_a_raw_value_read_as_an_empty_map() {
        let vault = tempdir().unwrap();
        let root = vault.path();
        seed_paper(root, "alpha");
        // v10 → v12 migration leaves the `{}` column default in place.
        let values = paper_values_at(root).unwrap();
        assert!(values["valuesByPath"].as_object().unwrap().is_empty());
    }
}
