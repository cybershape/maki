use agent_client_protocol_schema::ProtocolVersion;
use agent_client_protocol_schema::v1::{
    AgentCapabilities, Implementation, InitializeResponse, LoadSessionResponse, McpCapabilities,
    NewSessionResponse, PromptCapabilities, SessionCapabilities, SessionConfigOption,
    SessionConfigOptionCategory, SessionConfigSelectOption, SessionListCapabilities, SessionMode,
    SessionModeId, SessionModeState,
};
use maki_providers::{Model, ThinkingConfig, model_registry};

const VERSION: &str = env!("CARGO_PKG_VERSION");

const MODE_BUILD: &str = "build";
const MODE_PLAN: &str = "plan";

pub const MODEL_CONFIG_ID: &str = "model";
pub const THOUGHT_LEVEL_CONFIG_ID: &str = "thought_level";

pub fn is_thought_level_config_id(id: &str) -> bool {
    matches!(
        id,
        "thought_level" | "reasoning_effort" | "thinking" | "effort"
    )
}

pub fn initialize_response() -> InitializeResponse {
    InitializeResponse::new(ProtocolVersion::V1)
        .agent_capabilities(
            AgentCapabilities::new()
                .load_session(true)
                .prompt_capabilities(PromptCapabilities::new().image(true).embedded_context(true))
                .mcp_capabilities(McpCapabilities::new().http(true))
                .session_capabilities(
                    SessionCapabilities::new().list(SessionListCapabilities::new()),
                ),
        )
        .auth_methods(vec![])
        .agent_info(Implementation::new("maki", VERSION))
}

pub fn mode_state(current: &str) -> SessionModeState {
    SessionModeState::new(
        SessionModeId::from(current.to_string()),
        vec![
            SessionMode::new(SessionModeId::from(MODE_BUILD.to_string()), "Build"),
            SessionMode::new(SessionModeId::from(MODE_PLAN.to_string()), "Plan"),
        ],
    )
}

pub fn new_session_response(session_id: &str) -> NewSessionResponse {
    NewSessionResponse::new(session_id.to_string()).modes(mode_state(MODE_BUILD))
}

pub fn load_session_response() -> LoadSessionResponse {
    LoadSessionResponse::new().modes(mode_state(MODE_BUILD))
}

pub fn model_config_option(current: &str, specs: &[String]) -> SessionConfigOption {
    let mut options: Vec<SessionConfigSelectOption> =
        specs.iter().map(|spec| model_select_option(spec)).collect();
    if !specs.iter().any(|spec| spec == current) {
        options.insert(0, model_select_option(current));
    }
    SessionConfigOption::select(MODEL_CONFIG_ID, "Model", current.to_string(), options)
        .category(SessionConfigOptionCategory::Model)
}

fn model_select_option(spec: &str) -> SessionConfigSelectOption {
    let name = model_display_name(spec);
    let has_display_name = name != spec;
    let mut option = SessionConfigSelectOption::new(spec.to_string(), name);
    if has_display_name {
        option = option.description(spec.to_string());
    }
    option
}

fn model_display_name(spec: &str) -> String {
    Model::from_spec(spec)
        .ok()
        .and_then(|m| m.name)
        .or_else(|| model_registry::resolve_model_name_for_spec(spec))
        .unwrap_or_else(|| spec.to_string())
}

pub fn thought_level_config_option(
    model: &Model,
    thinking: ThinkingConfig,
) -> Option<SessionConfigOption> {
    if !model.supports_thinking() {
        return None;
    }
    let thinking_options = model.thinking_options();
    if thinking_options.is_empty() {
        return None;
    }
    let current = thinking.clamped(model).shown(model).to_string();
    let mut options: Vec<SessionConfigSelectOption> = thinking_options
        .iter()
        .map(|opt| SessionConfigSelectOption::new(opt.name.to_string(), thinking_label(opt.name)))
        .collect();
    if !thinking_options.iter().any(|opt| opt.name == current) {
        options.insert(
            0,
            SessionConfigSelectOption::new(current.clone(), thinking_label(&current)),
        );
    }
    Some(
        SessionConfigOption::select(
            THOUGHT_LEVEL_CONFIG_ID,
            "Reasoning Effort",
            current,
            options,
        )
        .category(SessionConfigOptionCategory::ThoughtLevel),
    )
}

pub fn session_config_options(
    model: &Model,
    specs: &[String],
    thinking: ThinkingConfig,
) -> Vec<SessionConfigOption> {
    let mut options = vec![model_config_option(&model.spec(), specs)];
    if let Some(thought_opt) = thought_level_config_option(model, thinking) {
        options.push(thought_opt);
    }
    options
}

fn thinking_label(name: &str) -> String {
    match name {
        "off" => "Off".into(),
        "adaptive" => "Adaptive".into(),
        "minimal" => "Minimal".into(),
        "low" => "Low".into(),
        "medium" => "Medium".into(),
        "high" => "High".into(),
        "xhigh" => "Extra High".into(),
        "max" => "Max".into(),
        other => other.to_string(),
    }
}

pub fn mode_id_to_agent_mode(mode_id: &str) -> Option<maki_agent::AgentMode> {
    match mode_id {
        MODE_BUILD => Some(maki_agent::AgentMode::Build),
        MODE_PLAN => {
            let storage = maki_storage::StateDir::resolve().ok()?;
            let plan_path = maki_storage::plans::new_plan_path(&storage).ok()?;
            Some(maki_agent::AgentMode::Plan(plan_path))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use agent_client_protocol_schema::v1::{SessionConfigKind, SessionConfigSelectOptions};
    use maki_providers::ModelInfo;
    use maki_storage::sessions::Effort;
    use test_case::test_case;

    const THINKING_SPEC: &str = "responses/gpt-5.6-sol";

    #[test_case("thought_level", true)]
    #[test_case("reasoning_effort", true)]
    #[test_case("thinking", true)]
    #[test_case("effort", true)]
    #[test_case("model", false)]
    #[test_case("mode", false)]
    #[test_case("unknown", false)]
    fn test_is_thought_level_config_id(id: &str, expected: bool) {
        assert_eq!(is_thought_level_config_id(id), expected);
    }

    #[test]
    fn thought_level_option_for_thinking_model() {
        let model = Model::from_spec(THINKING_SPEC).expect("a valid model");
        assert!(model.supports_thinking());

        let opt = thought_level_config_option(&model, ThinkingConfig::Effort(Effort::High))
            .expect("should produce thought level option");
        assert_eq!(opt.id.0.as_ref(), THOUGHT_LEVEL_CONFIG_ID);
        assert_eq!(opt.name, "Reasoning Effort");
        assert_eq!(
            opt.category,
            Some(SessionConfigOptionCategory::ThoughtLevel)
        );

        let SessionConfigKind::Select(select) = opt.kind else {
            panic!("expected select kind");
        };
        assert_eq!(select.current_value.0.as_ref(), "high");
        let SessionConfigSelectOptions::Ungrouped(options) = select.options else {
            panic!("expected ungrouped options");
        };
        let values: Vec<&str> = options.iter().map(|o| o.value.0.as_ref()).collect();
        assert!(values.contains(&"high"));
        assert!(values.contains(&"low"));
    }

    #[test]
    fn session_config_options_for_thinking_model() {
        let model = Model::from_spec(THINKING_SPEC).expect("a valid model");
        let specs = vec![THINKING_SPEC.to_string()];
        let opts = session_config_options(&model, &specs, ThinkingConfig::Effort(Effort::Medium));
        assert_eq!(opts.len(), 2);
        assert_eq!(opts[0].id.0.as_ref(), MODEL_CONFIG_ID);
        assert_eq!(opts[0].category, Some(SessionConfigOptionCategory::Model));
        assert_eq!(opts[1].id.0.as_ref(), THOUGHT_LEVEL_CONFIG_ID);
        assert_eq!(
            opts[1].category,
            Some(SessionConfigOptionCategory::ThoughtLevel)
        );
    }

    #[test]
    fn thought_level_option_omitted_when_unsupported() {
        let mut model = Model::from_spec(THINKING_SPEC).expect("a valid model");
        model.thinking_override = Some(maki_providers::model::ThinkingSupport::No);
        assert!(!model.supports_thinking());

        assert!(
            thought_level_config_option(&model, ThinkingConfig::Effort(Effort::High)).is_none()
        );
        let specs = vec![THINKING_SPEC.to_string()];
        let opts = session_config_options(&model, &specs, ThinkingConfig::Effort(Effort::High));
        assert_eq!(opts.len(), 1);
        assert_eq!(opts[0].id.0.as_ref(), MODEL_CONFIG_ID);
    }

    #[test]
    fn model_config_option_resolves_display_name_and_description() {
        let slug = "responses";
        let model_id = "friendly-model";
        let spec = format!("{slug}/{model_id}");
        model_registry::set_known_models(
            slug,
            vec![ModelInfo {
                name: Some("Friendly Model Name".to_string()),
                ..ModelInfo::id_only(model_id.to_string())
            }],
        );

        let opt = model_config_option(&spec, std::slice::from_ref(&spec));
        let SessionConfigKind::Select(select) = opt.kind else {
            panic!("expected select kind");
        };
        let SessionConfigSelectOptions::Ungrouped(options) = select.options else {
            panic!("expected ungrouped options");
        };
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].value.0.as_ref(), spec);
        assert_eq!(options[0].name, "Friendly Model Name");
        assert_eq!(options[0].description.as_deref(), Some(spec.as_str()));
    }

    #[test]
    fn model_config_option_falls_back_when_name_unknown() {
        let spec = "unknown-provider/unknown-model";
        let opt = model_config_option(spec, &[spec.to_string()]);
        let SessionConfigKind::Select(select) = opt.kind else {
            panic!("expected select kind");
        };
        let SessionConfigSelectOptions::Ungrouped(options) = select.options else {
            panic!("expected ungrouped options");
        };
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].value.0.as_ref(), spec);
        assert_eq!(options[0].name, spec);
        assert_eq!(options[0].description, None);
    }

    #[test]
    fn model_config_option_inserts_missing_current() {
        let current = "unknown-provider/current";
        let other = "unknown-provider/other";
        let opt = model_config_option(current, &[other.to_string()]);
        let SessionConfigKind::Select(select) = opt.kind else {
            panic!("expected select kind");
        };
        let SessionConfigSelectOptions::Ungrouped(options) = select.options else {
            panic!("expected ungrouped options");
        };
        assert_eq!(options.len(), 2);
        assert_eq!(options[0].value.0.as_ref(), current);
        assert_eq!(options[1].value.0.as_ref(), other);
    }
}
