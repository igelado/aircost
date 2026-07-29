//! Runtime model routing for Gemini tasks.
//!
//! Configuration precedence is deliberately explicit: built-in defaults are
//! overlaid by the TOML file named in [`GEMINI_CONFIG_PATH_ENV`], then by the
//! legacy environment variables, and finally by task-specific environment
//! variables. This preserves existing deployments while allowing every task
//! route to change without recompiling the application.

use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};

pub const GEMINI_CONFIG_PATH_ENV: &str = "AIRCOST_GEMINI_CONFIG";
pub const GEMINI_CONFIG_VERSION: u32 = 1;
pub const DEFAULT_GEMINI_CONFIG_PATH: &str = "config/gemini.toml";

const DEFAULT_LISTING_MODEL: &str = "gemini-3.5-flash-lite";
const DEFAULT_GROUNDED_MODEL: &str = "gemini-3.5-flash";
const DEFAULT_CURATION_MODEL: &str = "gemini-3.5-flash-lite";
const DEFAULT_VISUAL_MODEL: &str = "gemini-3.1-flash-lite";
const DEFAULT_GENERATE_CONTENT_MAX_OUTPUT_TOKENS: u64 = 4_096;
const DEFAULT_CURATION_MAX_OUTPUT_TOKENS: u64 = 12_000;
const DEFAULT_BENCHMARK_SAMPLE_SIZE: usize = 10;
const DEFAULT_BENCHMARK_SEED: u64 = 20_260_721;

/// A semantically distinct Gemini request made by the application.
///
/// Keeping these routes separate is intentional. For example, a cheap model
/// may be adequate for structure-only JSON conversion but not for grounded
/// catalog collision adjudication.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GeminiTask {
    ListingExtraction,
    GroundedMetadata,
    AvionicsIdentity,
    AvionicsReview,
    AvionicsSearchGrounding,
    AvionicsUrlVerification,
    AvionicsStructure,
    AvionicsCollisionStructure,
    AvionicsApprovedCandidateAdjudication,
    AircraftVisualIdentity,
    AircraftSearchGrounding,
    AircraftUrlVerification,
    AircraftStructure,
    AircraftCatalogAdjudication,
    AircraftHierarchyVerification,
}

impl GeminiTask {
    pub const ALL: [Self; 15] = [
        Self::ListingExtraction,
        Self::GroundedMetadata,
        Self::AvionicsIdentity,
        Self::AvionicsReview,
        Self::AvionicsSearchGrounding,
        Self::AvionicsUrlVerification,
        Self::AvionicsStructure,
        Self::AvionicsCollisionStructure,
        Self::AvionicsApprovedCandidateAdjudication,
        Self::AircraftVisualIdentity,
        Self::AircraftSearchGrounding,
        Self::AircraftUrlVerification,
        Self::AircraftStructure,
        Self::AircraftCatalogAdjudication,
        Self::AircraftHierarchyVerification,
    ];

    pub const fn environment_prefix(self) -> &'static str {
        match self {
            Self::ListingExtraction => "AIRCOST_GEMINI_LISTING_EXTRACTION",
            Self::GroundedMetadata => "AIRCOST_GEMINI_GROUNDED_METADATA",
            Self::AvionicsIdentity => "AIRCOST_GEMINI_AVIONICS_IDENTITY",
            Self::AvionicsReview => "AIRCOST_GEMINI_AVIONICS_REVIEW",
            Self::AvionicsSearchGrounding => "AIRCOST_GEMINI_AVIONICS_SEARCH_GROUNDING",
            Self::AvionicsUrlVerification => "AIRCOST_GEMINI_AVIONICS_URL_VERIFICATION",
            Self::AvionicsStructure => "AIRCOST_GEMINI_AVIONICS_STRUCTURE",
            Self::AvionicsCollisionStructure => "AIRCOST_GEMINI_AVIONICS_COLLISION_STRUCTURE",
            Self::AvionicsApprovedCandidateAdjudication => {
                "AIRCOST_GEMINI_AVIONICS_APPROVED_CANDIDATE_ADJUDICATION"
            }
            Self::AircraftVisualIdentity => "AIRCOST_GEMINI_AIRCRAFT_VISUAL_IDENTITY",
            Self::AircraftSearchGrounding => "AIRCOST_GEMINI_AIRCRAFT_SEARCH_GROUNDING",
            Self::AircraftUrlVerification => "AIRCOST_GEMINI_AIRCRAFT_URL_VERIFICATION",
            Self::AircraftStructure => "AIRCOST_GEMINI_AIRCRAFT_STRUCTURE",
            Self::AircraftCatalogAdjudication => "AIRCOST_GEMINI_AIRCRAFT_CATALOG_ADJUDICATION",
            Self::AircraftHierarchyVerification => "AIRCOST_GEMINI_AIRCRAFT_HIERARCHY_VERIFICATION",
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ListingExtraction => "listing_extraction",
            Self::GroundedMetadata => "grounded_metadata",
            Self::AvionicsIdentity => "avionics_identity",
            Self::AvionicsReview => "avionics_review",
            Self::AvionicsSearchGrounding => "avionics_search_grounding",
            Self::AvionicsUrlVerification => "avionics_url_verification",
            Self::AvionicsStructure => "avionics_structure",
            Self::AvionicsCollisionStructure => "avionics_collision_structure",
            Self::AvionicsApprovedCandidateAdjudication => {
                "avionics_approved_candidate_adjudication"
            }
            Self::AircraftVisualIdentity => "aircraft_visual_identity",
            Self::AircraftSearchGrounding => "aircraft_search_grounding",
            Self::AircraftUrlVerification => "aircraft_url_verification",
            Self::AircraftStructure => "aircraft_structure",
            Self::AircraftCatalogAdjudication => "aircraft_catalog_adjudication",
            Self::AircraftHierarchyVerification => "aircraft_hierarchy_verification",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// Do not put a thinking-level field on the wire.
    #[serde(rename = "none")]
    Disabled,
    Minimal,
    Low,
    Medium,
    High,
}

impl ThinkingLevel {
    pub const fn as_wire_value(self) -> Option<&'static str> {
        match self {
            Self::Disabled => None,
            Self::Minimal => Some("minimal"),
            Self::Low => Some("low"),
            Self::Medium => Some("medium"),
            Self::High => Some("high"),
        }
    }
}

/// The effective route for one task after file and environment overlays.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct TaskRoute {
    pub model: String,
    /// Optional escalation model used only by workflows after a primary
    /// response deterministically fails that stage's validation gates.
    pub fallback_model: Option<String>,
    /// Thinking level for the fallback model. `None` inherits the primary
    /// route's thinking level.
    pub fallback_thinking_level: Option<ThinkingLevel>,
    /// `None` preserves the APIs' current behavior of omitting service tier.
    pub service_tier: Option<String>,
    pub thinking_level: ThinkingLevel,
    pub max_output_tokens: u64,
}

impl TaskRoute {
    fn new(model: &str, thinking_level: ThinkingLevel, max_output_tokens: u64) -> Self {
        Self {
            model: model.to_string(),
            fallback_model: None,
            fallback_thinking_level: None,
            service_tier: None,
            thinking_level,
            max_output_tokens,
        }
    }

    fn apply(&mut self, overlay: TaskRouteOverlay) {
        if let Some(model) = overlay.model {
            self.model = model;
        }
        if let Some(fallback_model) = overlay.fallback_model {
            self.fallback_model = optional_model(fallback_model);
        }
        if let Some(fallback_thinking_level) = overlay.fallback_thinking_level {
            self.fallback_thinking_level = Some(fallback_thinking_level);
        }
        if let Some(service_tier) = overlay.service_tier {
            self.service_tier = service_tier_option(service_tier);
        }
        if let Some(thinking_level) = overlay.thinking_level {
            self.thinking_level = thinking_level;
        }
        if let Some(max_output_tokens) = overlay.max_output_tokens {
            self.max_output_tokens = max_output_tokens;
        }
    }

    fn validate(&self, task: GeminiTask) -> Result<()> {
        validate_model(&self.model)
            .with_context(|| format!("invalid primary model for {task:?}"))?;
        if let Some(fallback_model) = self.fallback_model.as_deref() {
            validate_model(fallback_model)
                .with_context(|| format!("invalid fallback model for {task:?}"))?;
        }
        if self.max_output_tokens == 0 {
            bail!("max_output_tokens for {task:?} must be positive");
        }
        if let Some(service_tier) = self.service_tier.as_deref() {
            validate_service_tier(service_tier)
                .with_context(|| format!("invalid service tier for {task:?}"))?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct TaskRouteOverlay {
    model: Option<String>,
    fallback_model: Option<String>,
    fallback_thinking_level: Option<ThinkingLevel>,
    service_tier: Option<String>,
    thinking_level: Option<ThinkingLevel>,
    max_output_tokens: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFile {
    version: u32,
    #[serde(default)]
    tasks: BTreeMap<GeminiTask, TaskRouteOverlay>,
    #[serde(default)]
    benchmark: BenchmarkConfig,
}

/// Reproducible selection of listings and task matrices for model comparison.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkConfig {
    #[serde(default = "default_benchmark_sample_size")]
    pub sample_size: usize,
    #[serde(default = "default_benchmark_seed")]
    pub seed: u64,
    /// When non-empty, these IDs replace deterministic sampling.
    #[serde(default)]
    pub listing_ids: Vec<i64>,
    #[serde(default)]
    pub matrices: Vec<BenchmarkMatrix>,
}

impl Default for BenchmarkConfig {
    fn default() -> Self {
        Self {
            sample_size: DEFAULT_BENCHMARK_SAMPLE_SIZE,
            seed: DEFAULT_BENCHMARK_SEED,
            listing_ids: Vec::new(),
            matrices: Vec::new(),
        }
    }
}

/// A Cartesian benchmark matrix. Empty non-model dimensions inherit the
/// effective task route, keeping common model-only comparisons compact.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkMatrix {
    pub task: GeminiTask,
    pub models: Vec<String>,
    #[serde(default)]
    pub service_tiers: Vec<String>,
    #[serde(default)]
    pub thinking_levels: Vec<ThinkingLevel>,
    #[serde(default)]
    pub max_output_tokens: Vec<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BenchmarkVariant {
    pub task: GeminiTask,
    pub route: TaskRoute,
}

/// Validated runtime configuration, ready for injection into Gemini clients.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GeminiRuntimeConfig {
    pub version: u32,
    pub tasks: BTreeMap<GeminiTask, TaskRoute>,
    pub benchmark: BenchmarkConfig,
    #[serde(skip)]
    source_path: Option<PathBuf>,
}

impl Default for GeminiRuntimeConfig {
    fn default() -> Self {
        let mut tasks = BTreeMap::new();
        tasks.insert(
            GeminiTask::ListingExtraction,
            TaskRoute::new(
                DEFAULT_LISTING_MODEL,
                ThinkingLevel::Low,
                DEFAULT_GENERATE_CONTENT_MAX_OUTPUT_TOKENS,
            ),
        );
        for task in [
            GeminiTask::GroundedMetadata,
            GeminiTask::AvionicsIdentity,
            GeminiTask::AvionicsReview,
        ] {
            tasks.insert(
                task,
                TaskRoute::new(
                    DEFAULT_GROUNDED_MODEL,
                    ThinkingLevel::Low,
                    DEFAULT_GENERATE_CONTENT_MAX_OUTPUT_TOKENS,
                ),
            );
        }
        tasks.insert(
            GeminiTask::AircraftVisualIdentity,
            TaskRoute::new(
                DEFAULT_VISUAL_MODEL,
                ThinkingLevel::Low,
                DEFAULT_GENERATE_CONTENT_MAX_OUTPUT_TOKENS,
            ),
        );
        for task in [
            GeminiTask::AvionicsSearchGrounding,
            GeminiTask::AvionicsUrlVerification,
        ] {
            tasks.insert(
                task,
                TaskRoute::new(
                    DEFAULT_CURATION_MODEL,
                    ThinkingLevel::Low,
                    DEFAULT_CURATION_MAX_OUTPUT_TOKENS,
                ),
            );
        }
        for task in [
            GeminiTask::AircraftSearchGrounding,
            GeminiTask::AircraftUrlVerification,
        ] {
            let mut route = TaskRoute::new(
                DEFAULT_CURATION_MODEL,
                ThinkingLevel::Low,
                DEFAULT_CURATION_MAX_OUTPUT_TOKENS,
            );
            route.fallback_model = Some(DEFAULT_GROUNDED_MODEL.to_string());
            route.fallback_thinking_level = Some(ThinkingLevel::Medium);
            tasks.insert(task, route);
        }
        tasks.insert(
            GeminiTask::AvionicsStructure,
            TaskRoute::new(
                DEFAULT_CURATION_MODEL,
                ThinkingLevel::Low,
                DEFAULT_CURATION_MAX_OUTPUT_TOKENS,
            ),
        );
        let mut aircraft_structure = TaskRoute::new(
            DEFAULT_CURATION_MODEL,
            ThinkingLevel::Low,
            DEFAULT_CURATION_MAX_OUTPUT_TOKENS,
        );
        aircraft_structure.fallback_model = Some(DEFAULT_GROUNDED_MODEL.to_string());
        tasks.insert(GeminiTask::AircraftStructure, aircraft_structure);
        // Collision structure receives the same bounded publisher dossier,
        // enriched with capability and shortlist window-ranking hints. Its
        // bounded validation retry reuses the configured Lite route.
        tasks.insert(
            GeminiTask::AvionicsCollisionStructure,
            TaskRoute::new(
                DEFAULT_CURATION_MODEL,
                ThinkingLevel::Low,
                DEFAULT_CURATION_MAX_OUTPUT_TOKENS,
            ),
        );
        // This bounded adjudication compares listing evidence only with
        // graph-approved catalog identities. It deliberately uses no Search,
        // URL Context, or function tools.
        tasks.insert(
            GeminiTask::AvionicsApprovedCandidateAdjudication,
            TaskRoute::new(DEFAULT_CURATION_MODEL, ThinkingLevel::Low, 2_048),
        );
        let mut aircraft_catalog_adjudication = TaskRoute::new(
            DEFAULT_GROUNDED_MODEL,
            ThinkingLevel::Medium,
            DEFAULT_CURATION_MAX_OUTPUT_TOKENS,
        );
        // The primary adjudication can exhaust its combined thinking/output
        // budget after both required function results are already present.
        // Retry the immutable tools-disabled final history on the configured
        // fallback route with lower thinking rather than repeating either
        // function stage.
        aircraft_catalog_adjudication.fallback_model = Some(DEFAULT_GROUNDED_MODEL.to_string());
        aircraft_catalog_adjudication.fallback_thinking_level = Some(ThinkingLevel::Low);
        tasks.insert(
            GeminiTask::AircraftCatalogAdjudication,
            aircraft_catalog_adjudication,
        );
        tasks.insert(
            GeminiTask::AircraftHierarchyVerification,
            TaskRoute::new(
                DEFAULT_GROUNDED_MODEL,
                ThinkingLevel::Medium,
                DEFAULT_CURATION_MAX_OUTPUT_TOKENS,
            ),
        );
        Self {
            version: GEMINI_CONFIG_VERSION,
            tasks,
            benchmark: BenchmarkConfig::default(),
            source_path: None,
        }
    }
}

impl GeminiRuntimeConfig {
    /// Load defaults plus an optional TOML file and all process environment
    /// overlays. `AIRCOST_GEMINI_CONFIG` wins; otherwise the checked-in
    /// `config/gemini.toml` is loaded when present.
    pub fn from_environment() -> Result<Self> {
        let mut config = match process_environment(GEMINI_CONFIG_PATH_ENV)? {
            Some(path) if !path.trim().is_empty() => Self::from_path(path.trim())?,
            _ if Path::new(DEFAULT_GEMINI_CONFIG_PATH).is_file() => {
                Self::from_path(DEFAULT_GEMINI_CONFIG_PATH)?
            }
            _ => Self::default(),
        };
        config.apply_legacy_environment(&process_environment)?;
        for task in GeminiTask::ALL {
            config.apply_task_environment(task, &process_environment)?;
        }
        config.validate()?;
        Ok(config)
    }

    /// Load and validate a TOML file without consulting process environment.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let contents = fs::read_to_string(path)
            .with_context(|| format!("could not read Gemini config {}", path.display()))?;
        let mut config = Self::from_toml_str(&contents)
            .with_context(|| format!("invalid Gemini config {}", path.display()))?;
        config.source_path = Some(path.to_path_buf());
        Ok(config)
    }

    /// Parse and validate a partial TOML config over the compiled defaults.
    pub fn from_toml_str(contents: &str) -> Result<Self> {
        let file: ConfigFile = toml::from_str(contents).context("could not parse Gemini TOML")?;
        if file.version != GEMINI_CONFIG_VERSION {
            bail!(
                "unsupported Gemini config version {}; expected {}",
                file.version,
                GEMINI_CONFIG_VERSION
            );
        }
        let mut config = Self::default();
        for (task, overlay) in file.tasks {
            config
                .tasks
                .get_mut(&task)
                .expect("every GeminiTask must have a compiled default")
                .apply(overlay);
        }
        config.benchmark = file.benchmark;
        config.validate()?;
        Ok(config)
    }

    pub fn route(&self, task: GeminiTask) -> &TaskRoute {
        self.tasks
            .get(&task)
            .expect("validated config contains every Gemini task")
    }

    pub fn source_path(&self) -> Option<&Path> {
        self.source_path.as_deref()
    }

    /// Expand a configured task matrix into concrete routes. Task settings are
    /// inherited for every dimension left empty by the matrix.
    pub fn benchmark_variants(&self, task: GeminiTask) -> Result<Vec<BenchmarkVariant>> {
        let matrix = self
            .benchmark
            .matrices
            .iter()
            .find(|matrix| matrix.task == task)
            .ok_or_else(|| anyhow!("no benchmark matrix configured for {task:?}"))?;
        let baseline = self.route(task);
        let service_tiers = if matrix.service_tiers.is_empty() {
            vec![baseline.service_tier.clone()]
        } else {
            matrix
                .service_tiers
                .iter()
                .map(|value| service_tier_option(value.clone()))
                .collect()
        };
        let thinking_levels = if matrix.thinking_levels.is_empty() {
            vec![baseline.thinking_level]
        } else {
            matrix.thinking_levels.clone()
        };
        let max_output_tokens = if matrix.max_output_tokens.is_empty() {
            vec![baseline.max_output_tokens]
        } else {
            matrix.max_output_tokens.clone()
        };
        let mut variants = Vec::new();
        for model in &matrix.models {
            for service_tier in &service_tiers {
                for thinking_level in &thinking_levels {
                    for max_output_tokens in &max_output_tokens {
                        let mut route = baseline.clone();
                        route.model = model.clone();
                        route.service_tier = service_tier.clone();
                        route.thinking_level = *thinking_level;
                        route.max_output_tokens = *max_output_tokens;
                        route.validate(task)?;
                        variants.push(BenchmarkVariant { task, route });
                    }
                }
            }
        }
        Ok(variants)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != GEMINI_CONFIG_VERSION {
            bail!("runtime Gemini config has an unsupported version");
        }
        for task in GeminiTask::ALL {
            self.tasks
                .get(&task)
                .ok_or_else(|| anyhow!("Gemini config is missing the {task:?} route"))?
                .validate(task)?;
        }
        if self.tasks.len() != GeminiTask::ALL.len() {
            bail!("Gemini config contains an unknown task route");
        }
        self.validate_benchmark()
    }

    fn validate_benchmark(&self) -> Result<()> {
        if self.benchmark.sample_size == 0 && self.benchmark.listing_ids.is_empty() {
            bail!("benchmark sample_size must be positive when listing_ids is empty");
        }
        let mut listing_ids = BTreeSet::new();
        for listing_id in &self.benchmark.listing_ids {
            if *listing_id <= 0 {
                bail!("benchmark listing IDs must be positive");
            }
            if !listing_ids.insert(*listing_id) {
                bail!("benchmark listing ID {listing_id} is duplicated");
            }
        }
        let mut matrix_tasks = BTreeSet::new();
        for matrix in &self.benchmark.matrices {
            if !matrix_tasks.insert(matrix.task) {
                bail!("benchmark has more than one matrix for {:?}", matrix.task);
            }
            if matrix.models.is_empty() {
                bail!("benchmark model matrix for {:?} is empty", matrix.task);
            }
            for model in &matrix.models {
                validate_model(model)
                    .with_context(|| format!("invalid benchmark model for {:?}", matrix.task))?;
            }
            for service_tier in &matrix.service_tiers {
                if !service_tier.trim().is_empty() {
                    validate_service_tier(service_tier).with_context(|| {
                        format!("invalid benchmark service tier for {:?}", matrix.task)
                    })?;
                }
            }
            if matrix.max_output_tokens.contains(&0) {
                bail!(
                    "benchmark max_output_tokens for {:?} must be positive",
                    matrix.task
                );
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn from_environment_reader<F>(read: F) -> Result<Self>
    where
        F: Fn(&str) -> Result<Option<String>>,
    {
        let mut config = match read(GEMINI_CONFIG_PATH_ENV)? {
            Some(path) if !path.trim().is_empty() => Self::from_path(path.trim())?,
            _ => Self::default(),
        };
        config.apply_legacy_environment(&read)?;
        for task in GeminiTask::ALL {
            config.apply_task_environment(task, &read)?;
        }
        config.validate()?;
        Ok(config)
    }

    fn apply_legacy_environment<F>(&mut self, read: &F) -> Result<()>
    where
        F: Fn(&str) -> Result<Option<String>>,
    {
        if let Some(model) = read("AIRCOST_GEMINI_MODEL")? {
            self.set_models(&[GeminiTask::ListingExtraction], model);
        }
        if let Some(model) = read("AIRCOST_GEMINI_GROUNDING_MODEL")? {
            self.set_models(
                &[GeminiTask::GroundedMetadata, GeminiTask::AvionicsIdentity],
                model,
            );
        }
        if let Some(model) = read("AIRCOST_GEMINI_AVIONICS_REVIEW_MODEL")? {
            self.set_models(&[GeminiTask::AvionicsReview], model);
        }
        if let Some(model) = read("GEMINI_AIRCRAFT_VISUAL_MODEL")? {
            self.set_models(&[GeminiTask::AircraftVisualIdentity], model);
        }
        if let Some(value) = read("AIRCOST_GEMINI_THINKING_LEVEL")? {
            let thinking_level = parse_thinking_level(&value).with_context(|| {
                "invalid legacy AIRCOST_GEMINI_THINKING_LEVEL environment value"
            })?;
            self.set_thinking_levels(
                &[
                    GeminiTask::ListingExtraction,
                    GeminiTask::GroundedMetadata,
                    GeminiTask::AvionicsIdentity,
                    GeminiTask::AvionicsReview,
                ],
                thinking_level,
            );
        }
        if let Some(value) = read("AIRCOST_GEMINI_MAX_OUTPUT_TOKENS")? {
            let max_output_tokens = parse_positive_u64("AIRCOST_GEMINI_MAX_OUTPUT_TOKENS", &value)?;
            self.set_max_output_tokens(
                &[
                    GeminiTask::ListingExtraction,
                    GeminiTask::GroundedMetadata,
                    GeminiTask::AvionicsIdentity,
                    GeminiTask::AvionicsReview,
                ],
                max_output_tokens,
            );
        }
        Ok(())
    }

    fn apply_task_environment<F>(&mut self, task: GeminiTask, read: &F) -> Result<()>
    where
        F: Fn(&str) -> Result<Option<String>>,
    {
        let prefix = task.environment_prefix();
        let route = self
            .tasks
            .get_mut(&task)
            .expect("every GeminiTask must have a compiled default");
        if let Some(value) = read(&format!("{prefix}_MODEL"))? {
            route.model = value;
        }
        if let Some(value) = read(&format!("{prefix}_FALLBACK_MODEL"))? {
            route.fallback_model = optional_model(value);
        }
        if let Some(value) = read(&format!("{prefix}_FALLBACK_THINKING_LEVEL"))? {
            route.fallback_thinking_level = parse_optional_thinking_level(&value)
                .with_context(|| format!("invalid {prefix}_FALLBACK_THINKING_LEVEL"))?;
        }
        if let Some(value) = read(&format!("{prefix}_SERVICE_TIER"))? {
            route.service_tier = service_tier_option(value);
        }
        if let Some(value) = read(&format!("{prefix}_THINKING_LEVEL"))? {
            route.thinking_level = parse_thinking_level(&value)
                .with_context(|| format!("invalid {prefix}_THINKING_LEVEL"))?;
        }
        if let Some(value) = read(&format!("{prefix}_MAX_OUTPUT_TOKENS"))? {
            route.max_output_tokens =
                parse_positive_u64(&format!("{prefix}_MAX_OUTPUT_TOKENS"), &value)?;
        }
        Ok(())
    }

    fn set_models(&mut self, tasks: &[GeminiTask], model: String) {
        for task in tasks {
            self.tasks
                .get_mut(task)
                .expect("every GeminiTask must have a compiled default")
                .model = model.clone();
        }
    }

    fn set_thinking_levels(&mut self, tasks: &[GeminiTask], level: ThinkingLevel) {
        for task in tasks {
            self.tasks
                .get_mut(task)
                .expect("every GeminiTask must have a compiled default")
                .thinking_level = level;
        }
    }

    fn set_max_output_tokens(&mut self, tasks: &[GeminiTask], max_output_tokens: u64) {
        for task in tasks {
            self.tasks
                .get_mut(task)
                .expect("every GeminiTask must have a compiled default")
                .max_output_tokens = max_output_tokens;
        }
    }
}

fn process_environment(name: &str) -> Result<Option<String>> {
    match env::var(name) {
        Ok(value) => Ok(Some(value)),
        Err(env::VarError::NotPresent) => Ok(None),
        Err(env::VarError::NotUnicode(_)) => bail!("{name} must contain valid Unicode"),
    }
}

fn parse_thinking_level(value: &str) -> Result<ThinkingLevel> {
    match value.trim().to_ascii_lowercase().as_str() {
        "" | "none" | "disabled" => Ok(ThinkingLevel::Disabled),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        _ => bail!("thinking level must be none, minimal, low, medium, or high"),
    }
}

fn parse_optional_thinking_level(value: &str) -> Result<Option<ThinkingLevel>> {
    if value.trim().is_empty() || value.trim().eq_ignore_ascii_case("inherit") {
        Ok(None)
    } else {
        parse_thinking_level(value).map(Some)
    }
}

fn parse_positive_u64(name: &str, value: &str) -> Result<u64> {
    let parsed = value
        .trim()
        .parse::<u64>()
        .with_context(|| format!("{name} must be an unsigned integer"))?;
    if parsed == 0 {
        bail!("{name} must be positive");
    }
    Ok(parsed)
}

fn validate_model(model: &str) -> Result<()> {
    let model = canonical_model(model);
    if model.is_empty() {
        bail!("model must not be blank");
    }
    if !model.starts_with("gemini-") {
        bail!("model must be a Gemini model ID, optionally prefixed by models/");
    }
    if model.ends_with("-latest") {
        bail!("model must be pinned, not a -latest alias");
    }
    if !model
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        bail!("model contains unsupported characters");
    }
    Ok(())
}

fn canonical_model(model: &str) -> &str {
    model.trim().strip_prefix("models/").unwrap_or(model.trim())
}

fn validate_service_tier(service_tier: &str) -> Result<()> {
    let service_tier = service_tier.trim();
    if service_tier.is_empty() {
        bail!("service tier must not be blank");
    }
    if !matches!(
        service_tier,
        "unspecified" | "standard" | "flex" | "priority"
    ) {
        bail!("service tier must be unspecified, standard, flex, or priority");
    }
    Ok(())
}

fn service_tier_option(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value != "unspecified").then(|| value.to_string())
}

fn optional_model(value: String) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && !value.eq_ignore_ascii_case("none")).then(|| value.to_string())
}

const fn default_benchmark_sample_size() -> usize {
    DEFAULT_BENCHMARK_SAMPLE_SIZE
}

const fn default_benchmark_seed() -> u64 {
    DEFAULT_BENCHMARK_SEED
}

#[cfg(test)]
mod tests {
    use super::*;

    fn map_environment(
        values: BTreeMap<String, String>,
    ) -> impl Fn(&str) -> Result<Option<String>> {
        move |name| Ok(values.get(name).cloned())
    }

    #[test]
    fn defaults_match_benchmarked_model_routes() {
        let config = GeminiRuntimeConfig::default();
        assert_eq!(
            config.route(GeminiTask::ListingExtraction).model,
            "gemini-3.5-flash-lite"
        );
        assert_eq!(
            config.route(GeminiTask::AvionicsIdentity).model,
            "gemini-3.5-flash"
        );
        assert_eq!(
            config.route(GeminiTask::AircraftVisualIdentity).model,
            "gemini-3.1-flash-lite"
        );
        for task in [
            GeminiTask::AvionicsSearchGrounding,
            GeminiTask::AvionicsUrlVerification,
            GeminiTask::AvionicsStructure,
            GeminiTask::AvionicsCollisionStructure,
        ] {
            let route = config.route(task);
            assert_eq!(route.model, "gemini-3.5-flash-lite");
            assert_eq!(route.thinking_level, ThinkingLevel::Low);
            assert_eq!(route.fallback_model, None);
            assert_eq!(route.fallback_thinking_level, None);
            assert_eq!(route.max_output_tokens, 12_000);
        }
        assert_eq!(
            config
                .route(GeminiTask::AvionicsUrlVerification)
                .max_output_tokens,
            12_000
        );
        assert_eq!(
            config.route(GeminiTask::AvionicsStructure).thinking_level,
            ThinkingLevel::Low
        );
        let collision_structure = config.route(GeminiTask::AvionicsCollisionStructure);
        assert_eq!(collision_structure.model, "gemini-3.5-flash-lite");
        assert_eq!(collision_structure.thinking_level, ThinkingLevel::Low);
        assert_eq!(collision_structure.fallback_model, None);
        assert_eq!(collision_structure.max_output_tokens, 12_000);
        let approved_adjudication = config.route(GeminiTask::AvionicsApprovedCandidateAdjudication);
        assert_eq!(approved_adjudication.model, "gemini-3.5-flash-lite");
        assert_eq!(approved_adjudication.thinking_level, ThinkingLevel::Low);
        assert_eq!(approved_adjudication.fallback_model, None);
        assert_eq!(approved_adjudication.max_output_tokens, 2_048);
        assert_eq!(
            config.route(GeminiTask::AircraftSearchGrounding).model,
            "gemini-3.5-flash-lite"
        );
        assert_eq!(
            config
                .route(GeminiTask::AircraftSearchGrounding)
                .max_output_tokens,
            12_000
        );
        assert_eq!(
            config
                .route(GeminiTask::AircraftSearchGrounding)
                .fallback_model
                .as_deref(),
            Some("gemini-3.5-flash")
        );
        assert_eq!(
            config
                .route(GeminiTask::AircraftSearchGrounding)
                .fallback_thinking_level,
            Some(ThinkingLevel::Medium)
        );
        assert_eq!(
            config
                .route(GeminiTask::AircraftStructure)
                .fallback_model
                .as_deref(),
            Some("gemini-3.5-flash")
        );
        let aircraft_adjudication = config.route(GeminiTask::AircraftCatalogAdjudication);
        assert_eq!(aircraft_adjudication.model, "gemini-3.5-flash");
        assert_eq!(aircraft_adjudication.thinking_level, ThinkingLevel::Medium);
        assert_eq!(
            aircraft_adjudication.fallback_model.as_deref(),
            Some("gemini-3.5-flash")
        );
        assert_eq!(
            aircraft_adjudication.fallback_thinking_level,
            Some(ThinkingLevel::Low)
        );
        assert_eq!(config.tasks.len(), GeminiTask::ALL.len());
        config.validate().expect("defaults must validate");
    }

    #[test]
    fn checked_in_configs_keep_avionics_curation_retries_on_lite() {
        let config = GeminiRuntimeConfig::from_toml_str(include_str!("../../config/gemini.toml"))
            .expect("checked-in Gemini config must load");
        assert_lite_only_avionics_curation_routes(&config);

        let aircraft_adjudication = config.route(GeminiTask::AircraftCatalogAdjudication);
        assert_eq!(aircraft_adjudication.model, "gemini-3.5-flash");
        assert_eq!(aircraft_adjudication.thinking_level, ThinkingLevel::Medium);
        assert_eq!(
            aircraft_adjudication.fallback_model.as_deref(),
            Some("gemini-3.5-flash")
        );
        assert_eq!(
            aircraft_adjudication.fallback_thinking_level,
            Some(ThinkingLevel::Low)
        );

        let example =
            GeminiRuntimeConfig::from_toml_str(include_str!("../../config/gemini.example.toml"))
                .expect("example Gemini config must load");
        assert_lite_only_avionics_curation_routes(&example);
    }

    fn assert_lite_only_avionics_curation_routes(config: &GeminiRuntimeConfig) {
        for task in [
            GeminiTask::AvionicsSearchGrounding,
            GeminiTask::AvionicsUrlVerification,
            GeminiTask::AvionicsStructure,
            GeminiTask::AvionicsCollisionStructure,
        ] {
            let route = config.route(task);
            assert_eq!(route.model, "gemini-3.5-flash-lite");
            assert_eq!(route.thinking_level, ThinkingLevel::Low);
            assert_eq!(route.fallback_model, None);
            assert_eq!(route.fallback_thinking_level, None);
            assert_eq!(route.max_output_tokens, 12_000);
        }

        let approved_adjudication = config.route(GeminiTask::AvionicsApprovedCandidateAdjudication);
        assert_eq!(approved_adjudication.model, "gemini-3.5-flash-lite");
        assert_eq!(approved_adjudication.thinking_level, ThinkingLevel::Low);
        assert_eq!(approved_adjudication.fallback_model, None);
        assert_eq!(approved_adjudication.max_output_tokens, 2_048);
    }

    #[test]
    fn avionics_pipeline_tasks_have_stable_names_and_environment_routes() {
        let cases = [
            (
                GeminiTask::AvionicsSearchGrounding,
                "avionics_search_grounding",
                "AIRCOST_GEMINI_AVIONICS_SEARCH_GROUNDING",
            ),
            (
                GeminiTask::AvionicsUrlVerification,
                "avionics_url_verification",
                "AIRCOST_GEMINI_AVIONICS_URL_VERIFICATION",
            ),
            (
                GeminiTask::AvionicsStructure,
                "avionics_structure",
                "AIRCOST_GEMINI_AVIONICS_STRUCTURE",
            ),
            (
                GeminiTask::AvionicsCollisionStructure,
                "avionics_collision_structure",
                "AIRCOST_GEMINI_AVIONICS_COLLISION_STRUCTURE",
            ),
            (
                GeminiTask::AvionicsApprovedCandidateAdjudication,
                "avionics_approved_candidate_adjudication",
                "AIRCOST_GEMINI_AVIONICS_APPROVED_CANDIDATE_ADJUDICATION",
            ),
        ];
        let mut environment = BTreeMap::new();
        for (task, name, prefix) in cases {
            assert_eq!(task.as_str(), name);
            assert_eq!(task.environment_prefix(), prefix);
            assert_eq!(
                serde_json::from_str::<GeminiTask>(&format!("\"{name}\""))
                    .expect("task name should deserialize"),
                task
            );
            environment.insert(format!("{prefix}_MODEL"), "gemini-3.5-flash-lite".into());
            environment.insert(
                format!("{prefix}_FALLBACK_MODEL"),
                "gemini-3.5-flash".into(),
            );
            environment.insert(format!("{prefix}_FALLBACK_THINKING_LEVEL"), "high".into());
            environment.insert(format!("{prefix}_MAX_OUTPUT_TOKENS"), "6144".into());
        }

        let config = GeminiRuntimeConfig::from_environment_reader(map_environment(environment))
            .expect("avionics task environment routes should load");
        for (task, _, _) in cases {
            let route = config.route(task);
            assert_eq!(route.model, "gemini-3.5-flash-lite");
            assert_eq!(route.fallback_model.as_deref(), Some("gemini-3.5-flash"));
            assert_eq!(route.fallback_thinking_level, Some(ThinkingLevel::High));
            assert_eq!(route.max_output_tokens, 6_144);
        }
    }

    #[test]
    fn partial_toml_overlays_defaults_and_expands_matrix() {
        let config = GeminiRuntimeConfig::from_toml_str(
            r#"
version = 1

[tasks.avionics_identity]
model = "gemini-3.5-flash-lite"
fallback_model = "gemini-3.5-flash"
fallback_thinking_level = "medium"
service_tier = "flex"
thinking_level = "minimal"
max_output_tokens = 3072

[benchmark]
sample_size = 8
seed = 42

[[benchmark.matrices]]
task = "avionics_identity"
models = ["gemini-3.1-flash-lite", "gemini-3.5-flash-lite"]
service_tiers = ["unspecified", "flex"]
thinking_levels = ["minimal", "low"]
max_output_tokens = [2048, 4096]
"#,
        )
        .expect("config should parse");

        let route = config.route(GeminiTask::AvionicsIdentity);
        assert_eq!(route.model, "gemini-3.5-flash-lite");
        assert_eq!(route.fallback_model.as_deref(), Some("gemini-3.5-flash"));
        assert_eq!(route.fallback_thinking_level, Some(ThinkingLevel::Medium));
        assert_eq!(route.service_tier.as_deref(), Some("flex"));
        assert_eq!(route.thinking_level, ThinkingLevel::Minimal);
        assert_eq!(route.max_output_tokens, 3_072);
        assert_eq!(
            config.route(GeminiTask::ListingExtraction).model,
            DEFAULT_LISTING_MODEL
        );
        assert_eq!(
            config
                .benchmark_variants(GeminiTask::AvionicsIdentity)
                .expect("matrix should expand")
                .len(),
            16
        );
    }

    #[test]
    fn task_environment_wins_over_file_and_legacy_environment() {
        let directory = std::env::temp_dir();
        let path = directory.join(format!(
            "aircost-gemini-config-{}-{}.toml",
            std::process::id(),
            std::thread::current().name().unwrap_or("unnamed")
        ));
        fs::write(
            &path,
            r#"
version = 1
[tasks.avionics_identity]
model = "gemini-3.1-flash-lite"
thinking_level = "minimal"
"#,
        )
        .expect("test config should write");
        let environment = BTreeMap::from([
            (
                GEMINI_CONFIG_PATH_ENV.to_string(),
                path.to_string_lossy().into_owned(),
            ),
            (
                "AIRCOST_GEMINI_GROUNDING_MODEL".to_string(),
                "gemini-3.5-flash".to_string(),
            ),
            (
                "AIRCOST_GEMINI_AVIONICS_IDENTITY_MODEL".to_string(),
                "gemini-3.5-flash-lite".to_string(),
            ),
            (
                "AIRCOST_GEMINI_AVIONICS_IDENTITY_THINKING_LEVEL".to_string(),
                "low".to_string(),
            ),
        ]);
        let config = GeminiRuntimeConfig::from_environment_reader(map_environment(environment))
            .expect("environment config should load");
        fs::remove_file(&path).expect("test config should be removable");

        let route = config.route(GeminiTask::AvionicsIdentity);
        assert_eq!(route.model, "gemini-3.5-flash-lite");
        assert_eq!(route.thinking_level, ThinkingLevel::Low);
        assert_eq!(config.source_path(), Some(path.as_path()));
    }

    #[test]
    fn legacy_environment_preserves_existing_fan_out() {
        let environment = BTreeMap::from([
            (
                "AIRCOST_GEMINI_GROUNDING_MODEL".to_string(),
                "gemini-3.5-flash-lite".to_string(),
            ),
            (
                "AIRCOST_GEMINI_MAX_OUTPUT_TOKENS".to_string(),
                "2048".to_string(),
            ),
        ]);
        let config = GeminiRuntimeConfig::from_environment_reader(map_environment(environment))
            .expect("legacy variables should load");
        assert_eq!(
            config.route(GeminiTask::GroundedMetadata).model,
            "gemini-3.5-flash-lite"
        );
        assert_eq!(
            config.route(GeminiTask::AvionicsIdentity).model,
            "gemini-3.5-flash-lite"
        );
        assert_eq!(
            config.route(GeminiTask::AvionicsReview).model,
            DEFAULT_GROUNDED_MODEL
        );
        assert_eq!(
            config
                .route(GeminiTask::ListingExtraction)
                .max_output_tokens,
            2_048
        );
        assert_eq!(
            config.route(GeminiTask::AvionicsReview).max_output_tokens,
            2_048
        );
    }

    #[test]
    fn rejects_unknown_fields_unpinned_models_and_bad_matrices() {
        let unknown = GeminiRuntimeConfig::from_toml_str(
            r#"
version = 1
[tasks.listing_extraction]
temperature = 0
"#,
        )
        .expect_err("unknown field must fail");
        assert!(unknown.to_string().contains("parse Gemini TOML"));

        let latest = GeminiRuntimeConfig::from_toml_str(
            r#"
version = 1
[tasks.listing_extraction]
model = "gemini-flash-latest"
"#,
        )
        .expect_err("latest alias must fail");
        assert!(format!("{latest:#}").contains("pinned"));

        let latest_fallback = GeminiRuntimeConfig::from_toml_str(
            r#"
version = 1
[tasks.avionics_search_grounding]
fallback_model = "gemini-flash-latest"
"#,
        )
        .expect_err("latest fallback alias must fail");
        assert!(format!("{latest_fallback:#}").contains("fallback model"));

        let empty_matrix = GeminiRuntimeConfig::from_toml_str(
            r#"
version = 1
[[benchmark.matrices]]
task = "listing_extraction"
models = []
"#,
        )
        .expect_err("empty model matrix must fail");
        assert!(empty_matrix.to_string().contains("matrix"));
    }
}
