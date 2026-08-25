-- Canonical SQLite schema for the Rust AirCost services.
CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  email TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  auth_provider TEXT NOT NULL DEFAULT 'local',
  auth_subject TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- A contract row is installed only after every object for a migration exists.
-- Startup treats the exact version and fingerprint as an atomic completion
-- marker; object-name checks below that layer detect later schema damage.
-- Canonical reruns only seed absent receipts; they never repair provenance or
-- replace the original installation timestamp.
CREATE TABLE IF NOT EXISTS schema_migration_contracts (
  migration_name TEXT PRIMARY KEY,
  contract_version INTEGER NOT NULL,
  contract_fingerprint TEXT NOT NULL,
  installed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(migration_name)) > 0),
  CHECK (typeof(contract_version) = 'integer' AND contract_version > 0),
  CHECK (length(contract_fingerprint) = 64),
  CHECK (contract_fingerprint = lower(contract_fingerprint)),
  CHECK (contract_fingerprint NOT GLOB '*[^0-9a-f]*')
);

-- A marker-present rerun must reject incompatible provenance before any
-- CREATE IF NOT EXISTS statement can accept or heal later schema objects.
CREATE TEMP TABLE reference_catalog_cutover_contract_preflight (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO reference_catalog_cutover_contract_preflight (valid)
SELECT CASE WHEN EXISTS (
  SELECT 1
  FROM schema_migration_contracts
  WHERE migration_name = '20260819_reference_catalog_cutover'
    AND (
      contract_version IS NOT 1
      OR contract_fingerprint IS NOT
        'fe31ca0eaae57cfc4ba5c824679bd950fcb98e20d6dd3e686a477fd22d05aab5'
    )
) THEN 0 ELSE 1 END;
DROP TABLE reference_catalog_cutover_contract_preflight;


-- SQLite library builds do not expose the shell's sha3() helper. Keep the
-- canonical schema independently fail-closed by comparing the complete
-- expected object relation rather than relying on a shell-only SQL function.
CREATE TEMP TABLE reference_catalog_schema_expected_objects (
  object_key TEXT PRIMARY KEY,
  definition TEXT NOT NULL
);
INSERT INTO reference_catalog_schema_expected_objects (object_key, definition)
VALUES
('index:aircraft_designation_aliases:idx_aircraft_designation_aliases_scope', '1:c:0:createuniqueindexidx_aircraft_designation_aliases_scopeonaircraft_designation_aliases(aircraft_designation_id,normalized_alias,coalesce(aircraft_market_id,0)):0:1:aircraft_designation_id:0:BINARY:1,1:3:normalized_alias:0:BINARY:1,2:-2::0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_designation_aliases:sqlite_autoindex_aircraft_designation_aliases_1', '1:u:0::0:7:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_designation_aliases:sqlite_autoindex_aircraft_designation_aliases_2', '1:u:0::0:1:aircraft_designation_id:0:BINARY:1,1:3:normalized_alias:0:BINARY:1,2:6:aircraft_market_id:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_designation_identifiers:idx_aircraft_designation_identifiers_scope', '1:c:0:createuniqueindexidx_aircraft_designation_identifiers_scopeonaircraft_designation_identifiers(aircraft_designation_id,authority,identifier_kind,normalized_identifier_value,coalesce(aircraft_market_id,0)):0:1:aircraft_designation_id:0:BINARY:1,1:2:authority:0:BINARY:1,2:3:identifier_kind:0:BINARY:1,3:5:normalized_identifier_value:0:BINARY:1,4:-2::0:BINARY:1,5:-1::0:BINARY:0'),
('index:aircraft_designation_identifiers:sqlite_autoindex_aircraft_designation_identifiers_1', '1:u:0::0:9:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_designation_identifiers:sqlite_autoindex_aircraft_designation_identifiers_2', '1:u:0::0:1:aircraft_designation_id:0:BINARY:1,1:2:authority:0:BINARY:1,2:3:identifier_kind:0:BINARY:1,3:5:normalized_identifier_value:0:BINARY:1,4:8:aircraft_market_id:0:BINARY:1,5:-1::0:BINARY:0'),
('index:aircraft_designations:sqlite_autoindex_aircraft_designations_1', '1:u:0::0:5:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_designations:sqlite_autoindex_aircraft_designations_2', '1:u:0::0:1:aircraft_model_family_id:0:BINARY:1,1:3:normalized_official_designation:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_designations:sqlite_autoindex_aircraft_designations_3', '1:u:0::0:0:id:0:BINARY:1,1:1:aircraft_model_family_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_engine_catalog_models:sqlite_autoindex_aircraft_engine_catalog_models_1', '1:u:0::0:11:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_engine_catalog_models:sqlite_autoindex_aircraft_engine_catalog_models_2', '1:u:0::0:2:normalized_manufacturer_name:0:BINARY:1,1:4:normalized_model_name:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_engine_catalog_models:sqlite_autoindex_aircraft_engine_catalog_models_3', '1:u:0::0:6:normalized_identifier_authority:0:BINARY:1,1:7:identifier_kind:0:BINARY:1,2:9:normalized_authoritative_identifier:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_factory_packages:sqlite_autoindex_aircraft_factory_packages_1', '1:u:0::0:6:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_factory_packages:sqlite_autoindex_aircraft_factory_packages_2', '1:u:0::0:1:aircraft_model_family_id:0:BINARY:1,1:3:normalized_name:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_factory_packages:sqlite_autoindex_aircraft_factory_packages_3', '1:u:0::0:0:id:0:BINARY:1,1:1:aircraft_model_family_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_family_aliases:idx_aircraft_family_aliases_scope', '1:c:0:createuniqueindexidx_aircraft_family_aliases_scopeonaircraft_family_aliases(aircraft_model_family_id,normalized_alias,coalesce(aircraft_market_id,0)):0:1:aircraft_model_family_id:0:BINARY:1,1:3:normalized_alias:0:BINARY:1,2:-2::0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_family_aliases:sqlite_autoindex_aircraft_family_aliases_1', '1:u:0::0:7:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_family_aliases:sqlite_autoindex_aircraft_family_aliases_2', '1:u:0::0:1:aircraft_model_family_id:0:BINARY:1,1:3:normalized_alias:0:BINARY:1,2:6:aircraft_market_id:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_feature_definitions:sqlite_autoindex_aircraft_feature_definitions_1', '1:u:0::0:1:feature_key:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_feature_definitions:sqlite_autoindex_aircraft_feature_definitions_2', '1:u:0::0:5:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_generation_designations:sqlite_autoindex_aircraft_generation_designations_1', '1:u:0::0:2:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_generation_designations:sqlite_autoindex_aircraft_generation_designations_2', '1:pk:0::0:0:aircraft_generation_id:0:BINARY:1,1:1:aircraft_designation_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_generations:sqlite_autoindex_aircraft_generations_1', '1:u:0::0:5:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_generations:sqlite_autoindex_aircraft_generations_2', '1:u:0::0:1:aircraft_model_family_id:0:BINARY:1,1:3:normalized_name:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_generations:sqlite_autoindex_aircraft_generations_3', '1:u:0::0:0:id:0:BINARY:1,1:1:aircraft_model_family_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_make_aliases:idx_aircraft_make_aliases_scope', '1:c:0:createuniqueindexidx_aircraft_make_aliases_scopeonaircraft_make_aliases(aircraft_make_id,normalized_alias,coalesce(aircraft_market_id,0)):0:1:aircraft_make_id:0:BINARY:1,1:3:normalized_alias:0:BINARY:1,2:-2::0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_make_aliases:sqlite_autoindex_aircraft_make_aliases_1', '1:u:0::0:7:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_make_aliases:sqlite_autoindex_aircraft_make_aliases_2', '1:u:0::0:1:aircraft_make_id:0:BINARY:1,1:3:normalized_alias:0:BINARY:1,2:6:aircraft_market_id:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_makes:sqlite_autoindex_aircraft_makes_1', '1:u:0::0:2:normalized_name:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_makes:sqlite_autoindex_aircraft_makes_2', '1:u:0::0:3:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_model_families:sqlite_autoindex_aircraft_model_families_1', '1:u:0::0:4:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_model_families:sqlite_autoindex_aircraft_model_families_2', '1:u:0::0:1:aircraft_make_id:0:BINARY:1,1:3:normalized_name:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_model_families:sqlite_autoindex_aircraft_model_families_3', '1:u:0::0:0:id:0:BINARY:1,1:1:aircraft_make_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_package_applicability:idx_aircraft_package_applicability_scope', '1:c:0:createuniqueindexidx_aircraft_package_applicability_scopeonaircraft_package_applicability(aircraft_factory_package_id,aircraft_designation_id,coalesce(aircraft_generation_id,0),coalesce(valid_from_model_year,0),coalesce(valid_to_model_year,0)):0:1:aircraft_factory_package_id:0:BINARY:1,1:2:aircraft_designation_id:0:BINARY:1,2:-2::0:BINARY:1,3:-2::0:BINARY:1,4:-2::0:BINARY:1,5:-1::0:BINARY:0'),
('index:aircraft_package_applicability:sqlite_autoindex_aircraft_package_applicability_1', '1:u:0::0:6:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_package_applicability:sqlite_autoindex_aircraft_package_applicability_2', '1:u:0::0:1:aircraft_factory_package_id:0:BINARY:1,1:2:aircraft_designation_id:0:BINARY:1,2:3:aircraft_generation_id:0:BINARY:1,3:4:valid_from_model_year:0:BINARY:1,4:5:valid_to_model_year:0:BINARY:1,5:-1::0:BINARY:0'),
('index:aircraft_propeller_catalog_models:sqlite_autoindex_aircraft_propeller_catalog_models_1', '1:u:0::0:11:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_propeller_catalog_models:sqlite_autoindex_aircraft_propeller_catalog_models_2', '1:u:0::0:2:normalized_manufacturer_name:0:BINARY:1,1:4:normalized_model_name:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_propeller_catalog_models:sqlite_autoindex_aircraft_propeller_catalog_models_3', '1:u:0::0:6:normalized_identifier_authority:0:BINARY:1,1:7:identifier_kind:0:BINARY:1,2:9:normalized_authoritative_identifier:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_reference_applicability_scopes:idx_aircraft_reference_scope_market', '0:c:0:createindexidx_aircraft_reference_scope_marketonaircraft_reference_applicability_scopes(aircraft_market_id,aircraft_serial_number_scheme_id,serial_from_sort_key,serial_to_sort_key):0:2:aircraft_market_id:0:BINARY:1,1:4:aircraft_serial_number_scheme_id:0:BINARY:1,2:8:serial_from_sort_key:0:BINARY:1,3:9:serial_to_sort_key:0:BINARY:1,4:-1::0:BINARY:0'),
('index:aircraft_reference_applicability_scopes:sqlite_autoindex_aircraft_reference_applicability_scopes_1', '1:u:0::0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:aircraft_market_id:0:BINARY:1,2:4:aircraft_serial_number_scheme_id:0:BINARY:1,3:5:serial_prefix:0:BINARY:1,4:8:serial_from_sort_key:0:BINARY:1,5:9:serial_to_sort_key:0:BINARY:1,6:-1::0:BINARY:0'),
('index:aircraft_reference_avionics:sqlite_autoindex_aircraft_reference_avionics_1', '1:u:0::0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:avionics_model_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_reference_configuration_versions:idx_aircraft_reference_versions_lookup', '0:c:0:createindexidx_aircraft_reference_versions_lookuponaircraft_reference_configuration_versions(aircraft_reference_configuration_id,model_year,publication_state,revision):0:1:aircraft_reference_configuration_id:0:BINARY:1,1:2:model_year:0:BINARY:1,2:5:publication_state:0:BINARY:1,3:3:revision:0:BINARY:1,4:-1::0:BINARY:0'),
('index:aircraft_reference_configuration_versions:sqlite_autoindex_aircraft_reference_configuration_versions_1', '1:u:0::0:6:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_reference_configuration_versions:sqlite_autoindex_aircraft_reference_configuration_versions_2', '1:u:0::0:1:aircraft_reference_configuration_id:0:BINARY:1,1:2:model_year:0:BINARY:1,2:3:revision:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_reference_configuration_versions:sqlite_autoindex_aircraft_reference_configuration_versions_3', '1:u:0::0:4:supersedes_version_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_reference_configurations:idx_aircraft_reference_config_base_generation', '1:c:1:createuniqueindexidx_aircraft_reference_config_base_generationonaircraft_reference_configurations(aircraft_designation_id,aircraft_generation_id)whereconfiguration_kind=''base''andaircraft_generation_idisnotnull:0:2:aircraft_designation_id:0:BINARY:1,1:3:aircraft_generation_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_reference_configurations:idx_aircraft_reference_config_base_no_generation', '1:c:1:createuniqueindexidx_aircraft_reference_config_base_no_generationonaircraft_reference_configurations(aircraft_designation_id)whereconfiguration_kind=''base''andaircraft_generation_idisnull:0:2:aircraft_designation_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_reference_configurations:idx_aircraft_reference_config_tier_generation', '1:c:1:createuniqueindexidx_aircraft_reference_config_tier_generationonaircraft_reference_configurations(aircraft_designation_id,aircraft_generation_id,tier_package_id)whereconfiguration_kind=''tier''andaircraft_generation_idisnotnull:0:2:aircraft_designation_id:0:BINARY:1,1:3:aircraft_generation_id:0:BINARY:1,2:4:tier_package_id:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_reference_configurations:idx_aircraft_reference_config_tier_no_generation', '1:c:1:createuniqueindexidx_aircraft_reference_config_tier_no_generationonaircraft_reference_configurations(aircraft_designation_id,tier_package_id)whereconfiguration_kind=''tier''andaircraft_generation_idisnull:0:2:aircraft_designation_id:0:BINARY:1,1:4:tier_package_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_reference_configurations:sqlite_autoindex_aircraft_reference_configurations_1', '1:u:0::0:7:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_reference_engines:sqlite_autoindex_aircraft_reference_engines_1', '1:u:0::0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:aircraft_engine_catalog_model_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_reference_fact_set_attestations:sqlite_autoindex_aircraft_reference_fact_set_attestations_1', '1:u:0::0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:fact_set_kind:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_reference_features:sqlite_autoindex_aircraft_reference_features_1', '1:u:0::0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:aircraft_feature_definition_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_reference_prices:sqlite_autoindex_aircraft_reference_prices_1', '1:u:0::0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:price_kind:0:BINARY:1,2:4:currency:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_reference_propellers:sqlite_autoindex_aircraft_reference_propellers_1', '1:u:0::0:1:aircraft_reference_configuration_version_id:0:BINARY:1,1:2:aircraft_propeller_catalog_model_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:aircraft_serial_number_schemes:sqlite_autoindex_aircraft_serial_number_schemes_1', '1:u:0::0:5:approval_decision_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:aircraft_serial_number_schemes:sqlite_autoindex_aircraft_serial_number_schemes_2', '1:u:0::0:1:aircraft_make_id:0:BINARY:1,1:2:name:0:BINARY:1,2:3:normalization_version:0:BINARY:1,3:-1::0:BINARY:0'),
('index:aircraft_valuation_compatibility_projections:idx_aircraft_valuation_projection_identity', '1:c:0:createuniqueindexidx_aircraft_valuation_projection_identityonaircraft_valuation_compatibility_projections(aircraft_make_id,aircraft_model_family_id,aircraft_designation_id,coalesce(aircraft_generation_id,0),coalesce(aircraft_factory_package_id,0)):0:1:aircraft_make_id:0:BINARY:1,1:2:aircraft_model_family_id:0:BINARY:1,2:3:aircraft_designation_id:0:BINARY:1,3:-2::0:BINARY:1,4:-2::0:BINARY:1,5:-1::0:BINARY:0'),
('index:avionics_models:idx_avionics_models_approved_manufacturer_name', '1:c:1:createuniqueindexidx_avionics_models_approved_manufacturer_nameonavionics_models(avionics_manufacturer_id,normalized_name)wherecatalog_status=''approved'':0:1:avionics_manufacturer_id:0:BINARY:1,1:3:normalized_name:0:BINARY:1,2:-1::0:BINARY:0'),
('index:avionics_models:idx_avionics_models_manufacturer_identifier', '1:c:1:createuniqueindexidx_avionics_models_manufacturer_identifieronavionics_models(avionics_manufacturer_id,manufacturer_identifier_kind,normalized_manufacturer_identifier)wherenormalized_manufacturer_identifierisnotnullandlength(trim(normalized_manufacturer_identifier))>0:0:1:avionics_manufacturer_id:0:BINARY:1,1:5:manufacturer_identifier_kind:0:BINARY:1,2:7:normalized_manufacturer_identifier:0:BINARY:1,3:-1::0:BINARY:0'),
('index:listing_verification_run_items:idx_listing_verification_run_items_claim', '0:c:0:createindexidx_listing_verification_run_items_claimonlisting_verification_run_items(run_id,status,position,id):0:1:run_id:0:BINARY:1,1:4:status:0:BINARY:1,2:3:position:0:BINARY:1,3:0:id:0:BINARY:1,4:-1::0:BINARY:0'),
('index:listing_verification_run_items:idx_listing_verification_run_items_one_active_listing', '1:c:1:createuniqueindexidx_listing_verification_run_items_one_active_listingonlisting_verification_run_items(listing_id)wherestatusin(''queued'',''running''):0:2:listing_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:listing_verification_run_items:idx_listing_verification_run_items_one_running_per_run', '1:c:1:createuniqueindexidx_listing_verification_run_items_one_running_per_runonlisting_verification_run_items(run_id)wherestatus=''running'':0:1:run_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:listing_verification_run_items:sqlite_autoindex_listing_verification_run_items_1', '1:u:0::0:1:run_id:0:BINARY:1,1:3:position:0:BINARY:1,2:-1::0:BINARY:0'),
('index:listing_verification_run_items:sqlite_autoindex_listing_verification_run_items_2', '1:u:0::0:1:run_id:0:BINARY:1,1:2:listing_id:0:BINARY:1,2:-1::0:BINARY:0'),
('index:official_dollar_normalization_facts:sqlite_autoindex_official_dollar_normalization_facts_1', '1:u:0::0:7:evidence_claim_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:official_dollar_normalization_facts:sqlite_autoindex_official_dollar_normalization_facts_2', '1:u:0::0:1:source_year:0:BINARY:1,1:2:target_year:0:BINARY:1,2:-1::0:BINARY:0'),
('index:plugin_submissions:idx_plugin_submissions_listing', '0:c:0:createindexidx_plugin_submissions_listingonplugin_submissions(canonical_listing_id):0:10:canonical_listing_id:0:BINARY:1,1:-1::0:BINARY:0'),
('index:plugin_submissions:idx_plugin_submissions_user', '0:c:0:createindexidx_plugin_submissions_useronplugin_submissions(user_id,submitted_at):0:1:user_id:0:BINARY:1,1:4:submitted_at:0:BINARY:1,2:-1::0:BINARY:0'),
('index:plugin_submissions:uq_plugin_submissions_signed_capture', '1:c:0:createuniqueindexuq_plugin_submissions_signed_captureonplugin_submissions(user_id,plugin_install_id,source_url,rendered_html_sha256):0:1:user_id:0:BINARY:1,1:2:plugin_install_id:0:BINARY:1,2:3:source_url:0:BINARY:1,3:6:rendered_html_sha256:0:BINARY:1,4:-1::0:BINARY:0'),
('table:aircraft_designation_aliases', 'createtableaircraft_designation_aliases(idintegerprimarykeyautoincrement,aircraft_designation_idintegernotnullreferencesaircraft_designations(id)ondeletecascade,aliastextnotnull,normalized_aliastextnotnull,valid_from_model_yearinteger,valid_to_model_yearinteger,aircraft_market_idintegerreferencesaircraft_markets(id)ondeleterestrict,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_designation_id,normalized_alias,aircraft_market_id),check(valid_from_model_yearisnullorvalid_to_model_yearisnullorvalid_to_model_year>=valid_from_model_year))'),
('table:aircraft_designation_identifiers', 'createtableaircraft_designation_identifiers(idintegerprimarykeyautoincrement,aircraft_designation_idintegernotnullreferencesaircraft_designations(id)ondeletecascade,authoritytextnotnull,identifier_kindtextnotnullcheck(identifier_kindin(''manufacturer_model_code'',''type_certificate_model'',''type_certificate_number'',''icao_type_designator'',''other_authoritative'')),identifier_valuetextnotnull,normalized_identifier_valuetextnotnull,valid_from_model_yearinteger,valid_to_model_yearinteger,aircraft_market_idintegerreferencesaircraft_markets(id)ondeleterestrict,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_designation_id,authority,identifier_kind,normalized_identifier_value,aircraft_market_id),check(length(trim(authority))>0),check(length(trim(normalized_identifier_value))>0),check(valid_from_model_yearisnullorvalid_to_model_yearisnullorvalid_to_model_year>=valid_from_model_year))'),
('table:aircraft_designations', 'createtableaircraft_designations(idintegerprimarykeyautoincrement,aircraft_model_family_idintegernotnullreferencesaircraft_model_families(id)ondeleterestrict,official_designationtextnotnull,normalized_official_designationtextnotnull,display_nametextnotnull,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,unique(aircraft_model_family_id,normalized_official_designation),unique(id,aircraft_model_family_id),check(length(trim(official_designation))>0),check(length(trim(normalized_official_designation))>0),check(length(trim(display_name))>0))'),
('table:aircraft_engine_catalog_models', 'createtableaircraft_engine_catalog_models(idintegerprimarykeyautoincrement,manufacturer_nametextnotnull,normalized_manufacturer_nametextnotnull,model_nametextnotnull,normalized_model_nametextnotnull,identifier_authoritytextnotnull,normalized_identifier_authoritytextnotnull,identifier_kindtextnotnullcheck(identifier_kindin(''manufacturer_model_code'',''regulator_model_designation'',''manufacturer_part_number'')),authoritative_identifiertextnotnull,normalized_authoritative_identifiertextnotnull,catalog_statustextnotnulldefault''approved''check(catalog_status=''approved''),approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,identity_evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,check(length(trim(manufacturer_name))>0),check(length(trim(normalized_manufacturer_name))>0),check(length(trim(model_name))>0),check(length(trim(normalized_model_name))>0),check(length(trim(identifier_authority))>0),check(length(trim(normalized_identifier_authority))>0),check(length(trim(authoritative_identifier))>0),check(length(trim(normalized_authoritative_identifier))>0),unique(normalized_manufacturer_name,normalized_model_name),unique(normalized_identifier_authority,identifier_kind,normalized_authoritative_identifier))'),
('table:aircraft_factory_packages', 'createtableaircraft_factory_packages(idintegerprimarykeyautoincrement,aircraft_model_family_idintegernotnullreferencesaircraft_model_families(id)ondeleterestrict,nametextnotnull,normalized_nametextnotnull,package_kindtextnotnullcheck(package_kindin(''trim_tier'',''option_bundle'',''special_edition'')),exclusivity_grouptext,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,unique(aircraft_model_family_id,normalized_name),unique(id,aircraft_model_family_id),check(package_kind<>''trim_tier''orlength(trim(exclusivity_group))>0))'),
('table:aircraft_family_aliases', 'createtableaircraft_family_aliases(idintegerprimarykeyautoincrement,aircraft_model_family_idintegernotnullreferencesaircraft_model_families(id)ondeletecascade,aliastextnotnull,normalized_aliastextnotnull,valid_from_model_yearinteger,valid_to_model_yearinteger,aircraft_market_idintegerreferencesaircraft_markets(id)ondeleterestrict,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_model_family_id,normalized_alias,aircraft_market_id),check(valid_from_model_yearisnullorvalid_to_model_yearisnullorvalid_to_model_year>=valid_from_model_year))'),
('table:aircraft_feature_definitions', 'createtableaircraft_feature_definitions(idintegerprimarykeyautoincrement,feature_keytextnotnullunique,display_nametextnotnull,value_typetextnotnullcheck(value_typein(''boolean'',''number'',''text'')),canonical_unittext,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,check((value_type=''number'')orcanonical_unitisnull))'),
('table:aircraft_generation_designations', 'createtableaircraft_generation_designations(aircraft_generation_idintegernotnullreferencesaircraft_generations(id)ondeletecascade,aircraft_designation_idintegernotnullreferencesaircraft_designations(id)ondeletecascade,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,primarykey(aircraft_generation_id,aircraft_designation_id))'),
('table:aircraft_generations', 'createtableaircraft_generations(idintegerprimarykeyautoincrement,aircraft_model_family_idintegernotnullreferencesaircraft_model_families(id)ondeleterestrict,nametextnotnull,normalized_nametextnotnull,ordinalintegercheck(ordinalisnullorordinal>=0),approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,unique(aircraft_model_family_id,normalized_name),unique(id,aircraft_model_family_id))'),
('table:aircraft_make_aliases', 'createtableaircraft_make_aliases(idintegerprimarykeyautoincrement,aircraft_make_idintegernotnullreferencesaircraft_makes(id)ondeletecascade,aliastextnotnull,normalized_aliastextnotnull,valid_from_model_yearinteger,valid_to_model_yearinteger,aircraft_market_idintegerreferencesaircraft_markets(id)ondeleterestrict,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_make_id,normalized_alias,aircraft_market_id),check(valid_from_model_yearisnullorvalid_from_model_yearbetween1900and2200),check(valid_to_model_yearisnullorvalid_to_model_yearbetween1900and2200),check(valid_from_model_yearisnullorvalid_to_model_yearisnullorvalid_to_model_year>=valid_from_model_year))'),
('table:aircraft_makes', 'createtableaircraft_makes(idintegerprimarykeyautoincrement,nametextnotnull,normalized_nametextnotnullunique,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,check(length(trim(name))>0),check(length(trim(normalized_name))>0))'),
('table:aircraft_model_families', 'createtableaircraft_model_families(idintegerprimarykeyautoincrement,aircraft_make_idintegernotnullreferencesaircraft_makes(id)ondeleterestrict,nametextnotnull,normalized_nametextnotnull,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,unique(aircraft_make_id,normalized_name),unique(id,aircraft_make_id),check(length(trim(name))>0),check(length(trim(normalized_name))>0))'),
('table:aircraft_package_applicability', 'createtableaircraft_package_applicability(idintegerprimarykeyautoincrement,aircraft_factory_package_idintegernotnullreferencesaircraft_factory_packages(id)ondeletecascade,aircraft_designation_idintegernotnullreferencesaircraft_designations(id)ondeletecascade,aircraft_generation_idintegerreferencesaircraft_generations(id)ondeletecascade,valid_from_model_yearinteger,valid_to_model_yearinteger,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_factory_package_id,aircraft_designation_id,aircraft_generation_id,valid_from_model_year,valid_to_model_year),check(valid_from_model_yearisnullorvalid_to_model_yearisnullorvalid_to_model_year>=valid_from_model_year))'),
('table:aircraft_propeller_catalog_models', 'createtableaircraft_propeller_catalog_models(idintegerprimarykeyautoincrement,manufacturer_nametextnotnull,normalized_manufacturer_nametextnotnull,model_nametextnotnull,normalized_model_nametextnotnull,identifier_authoritytextnotnull,normalized_identifier_authoritytextnotnull,identifier_kindtextnotnullcheck(identifier_kindin(''manufacturer_model_code'',''regulator_model_designation'',''manufacturer_part_number'')),authoritative_identifiertextnotnull,normalized_authoritative_identifiertextnotnull,catalog_statustextnotnulldefault''approved''check(catalog_status=''approved''),approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,identity_evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,check(length(trim(manufacturer_name))>0),check(length(trim(normalized_manufacturer_name))>0),check(length(trim(model_name))>0),check(length(trim(normalized_model_name))>0),check(length(trim(identifier_authority))>0),check(length(trim(normalized_identifier_authority))>0),check(length(trim(authoritative_identifier))>0),check(length(trim(normalized_authoritative_identifier))>0),unique(normalized_manufacturer_name,normalized_model_name),unique(normalized_identifier_authority,identifier_kind,normalized_authoritative_identifier))'),
('table:aircraft_reference_applicability_scopes', 'createtableaircraft_reference_applicability_scopes(idintegerprimarykeyautoincrement,aircraft_reference_configuration_version_idintegernotnullreferencesaircraft_reference_configuration_versions(id)ondeletecascade,aircraft_market_idintegernotnullreferencesaircraft_markets(id)ondeleterestrict,applies_to_all_serialsintegernotnulldefault1check(applies_to_all_serialsin(0,1)),aircraft_serial_number_scheme_idintegerreferencesaircraft_serial_number_schemes(id)ondeleterestrict,serial_prefixtext,serial_from_displaytext,serial_to_displaytext,serial_from_sort_keytext,serial_to_sort_keytext,evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,check((applies_to_all_serials=1andaircraft_serial_number_scheme_idisnullandserial_prefixisnullandserial_from_displayisnullandserial_to_displayisnullandserial_from_sort_keyisnullandserial_to_sort_keyisnull)or(applies_to_all_serials=0andaircraft_serial_number_scheme_idisnotnullandserial_from_displayisnotnullandserial_to_displayisnotnullandserial_from_sort_keyisnotnullandserial_to_sort_keyisnotnullandserial_from_sort_key<=serial_to_sort_key)),unique(aircraft_reference_configuration_version_id,aircraft_market_id,aircraft_serial_number_scheme_id,serial_prefix,serial_from_sort_key,serial_to_sort_key))'),
('table:aircraft_reference_avionics', 'createtableaircraft_reference_avionics(idintegerprimarykeyautoincrement,aircraft_reference_configuration_version_idintegernotnullreferencesaircraft_reference_configuration_versions(id)ondeletecascade,avionics_model_idintegernotnullreferencesavionics_models(id)ondeleterestrict,quantityintegernotnullcheck(quantity>0),equipment_roletextnotnullcheck(equipment_rolein(''standard'',''included_in_tier'')),evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_reference_configuration_version_id,avionics_model_id))'),
('table:aircraft_reference_configuration_versions', 'createtableaircraft_reference_configuration_versions(idintegerprimarykeyautoincrement,aircraft_reference_configuration_idintegernotnullreferencesaircraft_reference_configurations(id)ondeleterestrict,model_yearintegernotnullcheck(model_yearbetween1900and2200),revisionintegernotnullcheck(revision>=1),supersedes_version_idintegerreferencesaircraft_reference_configuration_versions(id)ondeleterestrict,publication_statetextnotnulldefault''building''check(publication_statein(''building'',''published'',''superseded'')),approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,published_attext,superseded_attext,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_reference_configuration_id,model_year,revision),unique(supersedes_version_id),check(supersedes_version_idisnullorsupersedes_version_id<>id),check((publication_state=''building''andpublished_atisnullandsuperseded_atisnull)or(publication_state=''published''andpublished_atisnotnullandsuperseded_atisnull)or(publication_state=''superseded''andpublished_atisnotnullandsuperseded_atisnotnull)))'),
('table:aircraft_reference_configurations', 'createtableaircraft_reference_configurations(idintegerprimarykeyautoincrement,aircraft_model_family_idintegernotnull,aircraft_designation_idintegernotnull,aircraft_generation_idinteger,tier_package_idinteger,configuration_kindtextnotnullcheck(configuration_kindin(''base'',''tier'')),display_nametextnotnull,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,foreignkey(aircraft_designation_id,aircraft_model_family_id)referencesaircraft_designations(id,aircraft_model_family_id)ondeleterestrict,foreignkey(aircraft_generation_id,aircraft_model_family_id)referencesaircraft_generations(id,aircraft_model_family_id)ondeleterestrict,foreignkey(tier_package_id,aircraft_model_family_id)referencesaircraft_factory_packages(id,aircraft_model_family_id)ondeleterestrict,check((configuration_kind=''base''andtier_package_idisnull)or(configuration_kind=''tier''andtier_package_idisnotnull)))'),
('table:aircraft_reference_engines', 'createtableaircraft_reference_engines(idintegerprimarykeyautoincrement,aircraft_reference_configuration_version_idintegernotnullreferencesaircraft_reference_configuration_versions(id)ondeletecascade,aircraft_engine_catalog_model_idintegernotnullreferencesaircraft_engine_catalog_models(id)ondeleterestrict,quantityintegernotnullcheck(quantity>0),equipment_roletextnotnullcheck(equipment_rolein(''standard'',''included_in_tier'')),evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_reference_configuration_version_id,aircraft_engine_catalog_model_id))'),
('table:aircraft_reference_fact_set_attestations', 'createtableaircraft_reference_fact_set_attestations(idintegerprimarykeyautoincrement,aircraft_reference_configuration_version_idintegernotnullreferencesaircraft_reference_configuration_versions(id)ondeletecascade,fact_set_kindtextnotnullcheck(fact_set_kindin(''avionics'',''engines'',''propellers'',''features'')),evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_reference_configuration_version_id,fact_set_kind))'),
('table:aircraft_reference_features', 'createtableaircraft_reference_features(idintegerprimarykeyautoincrement,aircraft_reference_configuration_version_idintegernotnullreferencesaircraft_reference_configuration_versions(id)ondeletecascade,aircraft_feature_definition_idintegernotnullreferencesaircraft_feature_definitions(id)ondeleterestrict,boolean_valueintegercheck(boolean_valueisnullorboolean_valuein(0,1)),number_valuereal,text_valuetext,evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_reference_configuration_version_id,aircraft_feature_definition_id),check((boolean_valueisnotnull)+(number_valueisnotnull)+(text_valueisnotnull)=1))'),
('table:aircraft_reference_prices', 'createtableaircraft_reference_prices(idintegerprimarykeyautoincrement,aircraft_reference_configuration_version_idintegernotnullreferencesaircraft_reference_configuration_versions(id)ondeletecascade,price_kindtextnotnullcheck(price_kindin(''base_msrp'',''equipped_msrp'',''tier_increment'',''other_factory_price'')),amountrealnotnullcheck(amount>0),currencytextnotnullcheck(length(currency)=3andcurrency=upper(currency)),price_reference_yearintegernotnullcheck(price_reference_yearbetween1900and2200),configuration_basistextnotnulldefault''unknown''check(configuration_basisin(''full_standard_configuration'',''base_aircraft_only'',''unknown'')),evidence_kindtextnotnullcheck(evidence_kindin(''direct_model_year'',''direct_other_year'',''interpolated'',''inferred'')),evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_reference_configuration_version_id,price_kind,currency))'),
('table:aircraft_reference_propellers', 'createtableaircraft_reference_propellers(idintegerprimarykeyautoincrement,aircraft_reference_configuration_version_idintegernotnullreferencesaircraft_reference_configuration_versions(id)ondeletecascade,aircraft_propeller_catalog_model_idintegernotnullreferencesaircraft_propeller_catalog_models(id)ondeleterestrict,quantityintegernotnullcheck(quantity>0),equipment_roletextnotnullcheck(equipment_rolein(''standard'',''included_in_tier'')),evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_reference_configuration_version_id,aircraft_propeller_catalog_model_id))'),
('table:aircraft_serial_number_schemes', 'createtableaircraft_serial_number_schemes(idintegerprimarykeyautoincrement,aircraft_make_idintegernotnullreferencesaircraft_makes(id)ondeleterestrict,nametextnotnull,normalization_versiontextnotnull,validation_patterntextnotnull,approval_decision_idintegernotnulluniquereferencesaircraft_identity_decisions(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(aircraft_make_id,name,normalization_version))'),
('table:aircraft_valuation_compatibility_projections', 'createtableaircraft_valuation_compatibility_projections(aircraft_model_variant_idintegerprimarykeyreferencesaircraft_model_variants(id)ondeleterestrict,aircraft_make_idintegernotnull,aircraft_model_family_idintegernotnull,aircraft_designation_idintegernotnull,aircraft_generation_idinteger,aircraft_factory_package_idinteger,created_from_aircraft_sale_listing_idintegernotnull,created_from_identity_assignment_idintegernotnull,identity_decision_idintegernotnullreferencesaircraft_identity_decisions(id)ondeleterestrict,identity_evidence_claim_idintegernotnullreferencescuration_evidence_claims(id)ondeleterestrict,faa_registry_snapshot_idintegernotnullreferencesfaa_registry_snapshots(id)ondeleterestrict,faa_n_numbertextnotnull,faa_source_record_sha256textnotnull,created_attextnotnulldefaultcurrent_timestamp,foreignkey(aircraft_model_family_id,aircraft_make_id)referencesaircraft_model_families(id,aircraft_make_id)ondeleterestrict,foreignkey(aircraft_designation_id,aircraft_model_family_id)referencesaircraft_designations(id,aircraft_model_family_id)ondeleterestrict,foreignkey(aircraft_generation_id,aircraft_model_family_id)referencesaircraft_generations(id,aircraft_model_family_id)ondeleterestrict,foreignkey(aircraft_factory_package_id,aircraft_model_family_id)referencesaircraft_factory_packages(id,aircraft_model_family_id)ondeleterestrict,foreignkey(faa_registry_snapshot_id,faa_n_number)referencesfaa_registry_aircraft(snapshot_id,n_number)ondeleterestrict,foreignkey(faa_registry_snapshot_id,faa_source_record_sha256)referencesfaa_registry_aircraft(snapshot_id,source_record_sha256)ondeleterestrict,check(aircraft_make_id>0),check(aircraft_model_family_id>0),check(aircraft_designation_id>0),check(aircraft_generation_idisnulloraircraft_generation_id>0),check(aircraft_factory_package_idisnulloraircraft_factory_package_id>0),check(created_from_aircraft_sale_listing_id>0),check(created_from_identity_assignment_id>0))'),
('table:avionics_models', 'createtableavionics_models(idintegerprimarykeyautoincrement,avionics_manufacturer_idintegernotnullreferencesavionics_manufacturers(id),nametextnotnull,normalized_nametextnotnull,catalog_statustextnotnulldefault''unreviewed''check(catalog_statusin(''unreviewed'',''approved'',''rejected'')),manufacturer_identifier_kindtextcheck(manufacturer_identifier_kindisnullormanufacturer_identifier_kindin(''manufacturer_part_number'',''manufacturer_model_number'',''sku'')),manufacturer_identifiertext,normalized_manufacturer_identifiertext,identity_source_urltext,identity_source_titletext,identity_evidence_texttext,identity_evidence_kindtextnotnulldefault''unreviewed''check(identity_evidence_kindin(''authoritative_reference'',''listing_only'',''unreviewed'')),identity_confidencetextcheck(identity_confidenceisnulloridentity_confidencein(''very_high'',''high'',''medium'',''low'')),catalog_reviewed_attext,introduced_yearinteger,discontinued_yearinteger,estimated_unit_value_usdreal,value_basistextnotnulldefault''unreviewed''check(value_basisin(''installed_contribution'',''replacement_cost'',''unreviewed'')),replacement_cost_usdreal,value_reference_yearinteger,value_sourcetext,valuation_scopetextnotnulldefault''unit''check(valuation_scopein(''unit'',''integrated_suite'')),created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,check((manufacturer_identifier_kindisnullandmanufacturer_identifierisnullandnormalized_manufacturer_identifierisnull)or(manufacturer_identifier_kindisnotnullandmanufacturer_identifierisnotnullandlength(trim(manufacturer_identifier))>0andnormalized_manufacturer_identifierisnotnullandlength(trim(normalized_manufacturer_identifier))>0)),check(catalog_status=''unreviewed''or(catalog_reviewed_atisnotnullandlength(trim(catalog_reviewed_at))>0)),check(catalog_status<>''approved''or(length(trim(name))>0andlength(trim(normalized_name))>0andlower(trim(normalized_name))notin(''unknown'',''generic'',''standard'',''factory'',''oem'',''various'',''multiple'',''avionics'',''avionicssuite'',''integratedavionics'',''integratedavionicssuite'',''glasspanel'',''flightinstruments'',''standardflightinstruments'',''standardvfravionics'',''standardifravionics'',''radio'',''radios'',''navcom'',''navigationsystem'',''gps'',''autopilot'',''transponder'',''adsb'',''weatherradar'',''audiopanel'',''display'',''equipment'')andinstr(''''||lower(trim(normalized_name))||'''',''series'')=0andinstr(''''||lower(trim(normalized_name))||'''',''family'')=0andmanufacturer_identifier_kindisnotnullandmanufacturer_identifierisnotnullandlength(trim(manufacturer_identifier))>0andnormalized_manufacturer_identifierisnotnullandlength(trim(normalized_manufacturer_identifier))>0andidentity_source_urlisnotnullandlength(trim(identity_source_url))>0andidentity_source_titleisnotnullandlength(trim(identity_source_title))>0andidentity_evidence_textisnotnullandlength(trim(identity_evidence_text))>0andidentity_evidence_kind=''authoritative_reference''andidentity_confidence=''very_high''andcatalog_reviewed_atisnotnullandlength(trim(catalog_reviewed_at))>0andlower(identity_source_url)notlike''%/listing/%''andlower(identity_source_url)notlike''%/listings/%''andlower(identity_source_url)notlike''%/aircraft-for-sale/%''andlower(identity_source_url)notlike''%/classifieds/%'')),check(value_basis<>''installed_contribution''or(estimated_unit_value_usd>=0andreplacement_cost_usd>=estimated_unit_value_usdandvalue_reference_yearbetween1900and2200andvalue_sourceisnotnullandlength(trim(value_source))>0)))'),
('table:listing_verification_run_items', 'createtablelisting_verification_run_items(idintegerprimarykeyautoincrement,run_idintegernotnullreferenceslisting_verification_runs(id)ondeletecascade,listing_idintegernotnullreferencesaircraft_sale_listings(id)ondeletecascade,positionintegernotnullcheck(position>=0),statustextnotnulldefault''queued''constraintlisting_verification_run_items_status_checkcheck(statusin(''queued'',''running'',''verified'',''pending_review'',''blocked'',''failed'',''cancelled'')),attempt_countintegernotnulldefault0check(attempt_count>=0),lease_tokentext,lease_expires_at_epoch_secondsinteger,outcome_jsontext,reason_codetext,reasontext,created_attextnotnulldefaultcurrent_timestamp,updated_attextnotnulldefaultcurrent_timestamp,started_attext,completed_attext,unique(run_id,position),unique(run_id,listing_id),check(lease_tokenisnullorlength(trim(lease_token))between1and200),check((status=''running''andlease_tokenisnotnullandlease_expires_at_epoch_secondsisnotnullandstarted_atisnotnullandcompleted_atisnull)or(status<>''running''andlease_tokenisnullandlease_expires_at_epoch_secondsisnull)),constraintlisting_verification_run_items_completion_checkcheck((statusin(''queued'',''running'')andcompleted_atisnull)or(statusin(''verified'',''pending_review'',''blocked'',''failed'',''cancelled'')andcompleted_atisnotnull)),check(outcome_jsonisnullor(length(outcome_json)between2and65536andjson_valid(outcome_json)andjson_type(outcome_json)=''object'')),constraintlisting_verification_run_items_outcome_required_checkcheck(statusnotin(''verified'',''pending_review'',''blocked'')oroutcome_jsonisnotnull),check(reason_codeisnullorlength(trim(reason_code))between1and100),check(reasonisnullorlength(trim(reason))between1and2000))'),
('table:official_dollar_normalization_facts', 'createtableofficial_dollar_normalization_facts(idintegerprimarykeyautoincrement,source_yearintegernotnullcheck(source_yearbetween1900and2200),target_yearintegernotnullcheck(target_yearbetween1900and2200),index_seriestextnotnullcheck(length(trim(index_series))>0),source_index_valuerealnotnullcheck(source_index_value>0),target_index_valuerealnotnullcheck(target_index_value>0),normalization_factorrealnotnullcheck(normalization_factor>0),evidence_claim_idintegernotnulluniquereferencescuration_evidence_claims(id)ondeleterestrict,created_attextnotnulldefaultcurrent_timestamp,unique(source_year,target_year),check(source_year<>target_year),check(abs(normalization_factor-(target_index_value/source_index_value))<=0.000000001))'),
('table:plugin_submissions', 'createtableplugin_submissions(idintegerprimarykeyautoincrement,user_idintegernotnullreferencesusers(id),plugin_install_idintegernotnullreferencesplugin_installs(id),source_urltextnotnull,submitted_attextnotnulldefaultcurrent_timestamp,rendered_htmltextnotnull,rendered_html_sha256textnotnull,signature_base64textnotnull,extracted_listing_jsontext,extraction_errortext,canonical_listing_idintegerreferencesaircraft_sale_listings(id)ondeletesetnull)'),
('trigger:aircraft_aliases_require_approval_designation', 'createtriggeraircraft_aliases_require_approval_designationbeforeinsertonaircraft_designation_aliaseswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''alias''andclaim.validation_status=''validated'')beginselectraise(abort,''aircraftaliasrequiresanapprovedevidence-backeddecision'');end'),
('trigger:aircraft_aliases_require_approval_family', 'createtriggeraircraft_aliases_require_approval_familybeforeinsertonaircraft_family_aliaseswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''alias''andclaim.validation_status=''validated'')beginselectraise(abort,''aircraftaliasrequiresanapprovedevidence-backeddecision'');end'),
('trigger:aircraft_aliases_require_approval_make', 'createtriggeraircraft_aliases_require_approval_makebeforeinsertonaircraft_make_aliaseswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''alias''andclaim.validation_status=''validated'')beginselectraise(abort,''aircraftaliasrequiresanapprovedevidence-backeddecision'');end'),
('trigger:aircraft_designations_require_approval', 'createtriggeraircraft_designations_require_approvalbeforeinsertonaircraft_designationswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''designation''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''aircraftdesignationrequiresanapprovedprimary-sourcedecision'');end'),
('trigger:aircraft_engine_catalog_models_immutable_delete', 'createtriggeraircraft_engine_catalog_models_immutable_deletebeforedeleteonaircraft_engine_catalog_modelsbeginselectraise(abort,''approvedenginecatalogmodelsareimmutable'');end'),
('trigger:aircraft_engine_catalog_models_immutable_update', 'createtriggeraircraft_engine_catalog_models_immutable_updatebeforeupdateonaircraft_engine_catalog_modelsbeginselectraise(abort,''approvedenginecatalogmodelsareimmutable'');end'),
('trigger:aircraft_engine_catalog_models_require_approval', 'createtriggeraircraft_engine_catalog_models_require_approvalbeforeinsertonaircraft_engine_catalog_modelswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdecision_claimondecision_claim.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=decision_claim.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''engine_model''anddecision_claim.evidence_claim_id=new.identity_evidence_claim_idanddecision_claim.evidence_rolein(''identity'',''specification'')andclaim.claim_kindin(''identity'',''specification'')andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''enginecatalogmodelrequiresanapprovedprimary-sourceidentifier'');end'),
('trigger:aircraft_families_require_approval', 'createtriggeraircraft_families_require_approvalbeforeinsertonaircraft_model_familieswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''family''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''aircraftfamilyrequiresanapprovedprimary-sourcedecision'');end'),
('trigger:aircraft_family_retrieval_key_validate_insert', 'createtriggeraircraft_family_retrieval_key_validate_insertbeforeinsertonaircraft_model_familiesbeginselectraise(abort,''aircraftfamilyrequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_family_retrieval_key_validate_update', 'createtriggeraircraft_family_retrieval_key_validate_updatebeforeupdateofname,normalized_nameonaircraft_model_familiesbeginselectraise(abort,''aircraftfamilyrequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_feature_definitions_require_approval', 'createtriggeraircraft_feature_definitions_require_approvalbeforeinsertonaircraft_feature_definitionswhennotexists(select1fromaircraft_identity_decisionswhereid=new.approval_decision_idanddecision_status=''approved''anddecision_action=''approve_new''andentity_kind=''feature_definition'')beginselectraise(abort,''featuredefinitionrequiresanapproveddecision'');end'),
('trigger:aircraft_generation_designations_require_approval', 'createtriggeraircraft_generation_designations_require_approvalbeforeinsertonaircraft_generation_designationswhennotexists(select1fromaircraft_identity_decisionswhereid=new.approval_decision_idanddecision_status=''approved''anddecision_action=''approve_new''andentity_kind=''generation_designation'')ornotexists(select1fromaircraft_generationsgenerationjoinaircraft_designationsdesignationondesignation.id=new.aircraft_designation_idwheregeneration.id=new.aircraft_generation_idandgeneration.aircraft_model_family_id=designation.aircraft_model_family_id)beginselectraise(abort,''generation/designationlinkrequiresapprovalwithinonefamily'');end'),
('trigger:aircraft_generation_retrieval_key_validate_insert', 'createtriggeraircraft_generation_retrieval_key_validate_insertbeforeinsertonaircraft_generationsbeginselectraise(abort,''aircraftgenerationrequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_generation_retrieval_key_validate_update', 'createtriggeraircraft_generation_retrieval_key_validate_updatebeforeupdateofname,normalized_nameonaircraft_generationsbeginselectraise(abort,''aircraftgenerationrequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_generations_require_approval', 'createtriggeraircraft_generations_require_approvalbeforeinsertonaircraft_generationswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''generation''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''aircraftgenerationrequiresanapprovedprimary-sourcedecision'');end'),
('trigger:aircraft_identifiers_require_approval', 'createtriggeraircraft_identifiers_require_approvalbeforeinsertonaircraft_designation_identifierswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''identifier''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''aircraftidentifierrequiresanapprovedprimary-sourcedecision'');end'),
('trigger:aircraft_make_alias_identity_collision', 'createtriggeraircraft_make_alias_identity_collisionbeforeinsertonaircraft_make_aliaseswhenexists(select1fromaircraft_make_aliasesexisting_aliasleftjoinaircraft_marketsexisting_marketonexisting_market.id=existing_alias.aircraft_market_idleftjoinaircraft_marketsnew_marketonnew_market.id=new.aircraft_market_idwhereexisting_alias.aircraft_make_id<>new.aircraft_make_idandexisting_alias.normalized_alias=new.normalized_aliasand(existing_alias.valid_to_model_yearisnullornew.valid_from_model_yearisnullorexisting_alias.valid_to_model_year>=new.valid_from_model_year)and(new.valid_to_model_yearisnullorexisting_alias.valid_from_model_yearisnullornew.valid_to_model_year>=existing_alias.valid_from_model_year)and(existing_alias.aircraft_market_idisnullornew.aircraft_market_idisnullorexisting_alias.aircraft_market_id=new.aircraft_market_idorexisting_market.code=''global''ornew_market.code=''global''))orexists(select1fromaircraft_makesother_makewhereother_make.id<>new.aircraft_make_idand(other_make.normalized_name=new.normalized_aliasorlower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(other_make.name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'',''''))=replace(new.normalized_alias,'''','''')))beginselectraise(abort,''aircraftmakealiasoverlapsanothercanonicalmakeinmarket/yearscope'');end'),
('trigger:aircraft_make_alias_identity_immutable_delete', 'createtriggeraircraft_make_alias_identity_immutable_deletebeforedeleteonaircraft_make_aliasesbeginselectraise(abort,''approvedaircraftmakealiasesareimmutable'');end'),
('trigger:aircraft_make_alias_identity_immutable_update', 'createtriggeraircraft_make_alias_identity_immutable_updatebeforeupdateonaircraft_make_aliasesbeginselectraise(abort,''approvedaircraftmakealiasesareimmutable'');end'),
('trigger:aircraft_make_alias_identity_key_validate', 'createtriggeraircraft_make_alias_identity_key_validatebeforeinsertonaircraft_make_aliaseswhennew.normalized_alias=''''ornew.normalized_alias<>trim(new.normalized_alias)ornew.normalized_alias<>lower(new.normalized_alias)ornew.normalized_aliasglob''*[^a-z0-9]*''orinstr(new.normalized_alias,'''')>0orreplace(new.normalized_alias,'''','''')<>lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(new.alias),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'',''''))beginselectraise(abort,''aircraftmakealiasrequiresitsdeterministicnormalizedretrievalkey'');end'),
('trigger:aircraft_make_alias_tcds_lineage_collision', 'createtriggeraircraft_make_alias_tcds_lineage_collisionbeforeinsertonaircraft_make_aliaseswhenexists(select1fromaircraft_tcds_make_lineage_bindingsbindingwherebinding.aircraft_make_id<>new.aircraft_make_idandlower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(new.alias),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'',''''))=lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(binding.faa_manufacturer_name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'','''')))beginselectraise(abort,''aircraftmakealiascollideswithanapprovedfaa/tcdslineagelabel'');end'),
('trigger:aircraft_make_identity_alias_collision_insert', 'createtriggeraircraft_make_identity_alias_collision_insertbeforeinsertonaircraft_makeswhenexists(select1fromaircraft_make_aliasesaliaswherelower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(new.name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'',''''))=replace(alias.normalized_alias,'''',''''))beginselectraise(abort,''canonicalaircraftmakecollideswithanapprovedalias'');end'),
('trigger:aircraft_make_identity_alias_collision_update', 'createtriggeraircraft_make_identity_alias_collision_updatebeforeupdateofname,normalized_nameonaircraft_makeswhenexists(select1fromaircraft_make_aliasesaliaswherealias.aircraft_make_id<>old.idandlower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(new.name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'',''''))=replace(alias.normalized_alias,'''',''''))beginselectraise(abort,''canonicalaircraftmakecollideswithanapprovedalias'');end'),
('trigger:aircraft_make_retrieval_key_validate_insert', 'createtriggeraircraft_make_retrieval_key_validate_insertbeforeinsertonaircraft_makesbeginselectraise(abort,''aircraftmakerequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_make_retrieval_key_validate_update', 'createtriggeraircraft_make_retrieval_key_validate_updatebeforeupdateofname,normalized_nameonaircraft_makesbeginselectraise(abort,''aircraftmakerequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_make_tcds_lineage_collision_insert', 'createtriggeraircraft_make_tcds_lineage_collision_insertbeforeinsertonaircraft_makeswhenexists(select1fromaircraft_tcds_make_lineage_bindingsbindingwherelower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(new.name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'',''''))=lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(binding.faa_manufacturer_name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'','''')))beginselectraise(abort,''canonicalaircraftmakecollideswithanapprovedfaa/tcdslineagelabel'');end'),
('trigger:aircraft_make_tcds_lineage_collision_update', 'createtriggeraircraft_make_tcds_lineage_collision_updatebeforeupdateofname,normalized_nameonaircraft_makeswhenexists(select1fromaircraft_tcds_make_lineage_bindingsbindingwherebinding.aircraft_make_id<>old.idandlower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(new.name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'',''''))=lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(binding.faa_manufacturer_name),'''',''''),''-'',''''),''.'',''''),''/'',''''),''_'',''''),'','',''''),''&'',''''),char(39),''''),''('',''''),'')'','''')))beginselectraise(abort,''canonicalaircraftmakecollideswithanapprovedfaa/tcdslineagelabel'');end'),
('trigger:aircraft_makes_require_approval', 'createtriggeraircraft_makes_require_approvalbeforeinsertonaircraft_makeswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdecision_claimondecision_claim.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=decision_claim.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''make''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''aircraftmakerequiresanapprovedprimary-sourcedecision'');end'),
('trigger:aircraft_package_applicability_require_approval', 'createtriggeraircraft_package_applicability_require_approvalbeforeinsertonaircraft_package_applicabilitywhennotexists(select1fromaircraft_identity_decisionswhereid=new.approval_decision_idanddecision_status=''approved''anddecision_action=''approve_new''andentity_kind=''package_applicability'')ornotexists(select1fromaircraft_factory_packagespackagejoinaircraft_designationsdesignationondesignation.id=new.aircraft_designation_idleftjoinaircraft_generationsgenerationongeneration.id=new.aircraft_generation_idwherepackage.id=new.aircraft_factory_package_idandpackage.aircraft_model_family_id=designation.aircraft_model_family_idand(new.aircraft_generation_idisnullorgeneration.aircraft_model_family_id=designation.aircraft_model_family_id))beginselectraise(abort,''packageapplicabilityrequiresapprovalwithinonefamily'');end'),
('trigger:aircraft_package_retrieval_key_validate_insert', 'createtriggeraircraft_package_retrieval_key_validate_insertbeforeinsertonaircraft_factory_packagesbeginselectraise(abort,''aircraftpackagerequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_package_retrieval_key_validate_update', 'createtriggeraircraft_package_retrieval_key_validate_updatebeforeupdateofname,normalized_nameonaircraft_factory_packagesbeginselectraise(abort,''aircraftpackagerequiresitsdeterministicretrievalkey'')wherenew.normalized_name<>(withrecursivenormalized(character_offset,normalized_name)as(values(1,'''')unionallselectcharacter_offset+1,casewhensubstr(new.name,character_offset,1)glob''[a-za-z0-9]''thennormalized_name||lower(substr(new.name,character_offset,1))whennormalized_name<>''''andsubstr(normalized_name,-1,1)<>''''thennormalized_name||''''elsenormalized_nameendfromnormalizedwherecharacter_offset<=length(new.name))selectrtrim(normalized_name)fromnormalizedwherecharacter_offset>length(new.name));end'),
('trigger:aircraft_packages_require_approval', 'createtriggeraircraft_packages_require_approvalbeforeinsertonaircraft_factory_packageswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''package''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''aircraftpackagerequiresanapprovedprimary-sourcedecision'');end'),
('trigger:aircraft_propeller_catalog_models_immutable_delete', 'createtriggeraircraft_propeller_catalog_models_immutable_deletebeforedeleteonaircraft_propeller_catalog_modelsbeginselectraise(abort,''approvedpropellercatalogmodelsareimmutable'');end'),
('trigger:aircraft_propeller_catalog_models_immutable_update', 'createtriggeraircraft_propeller_catalog_models_immutable_updatebeforeupdateonaircraft_propeller_catalog_modelsbeginselectraise(abort,''approvedpropellercatalogmodelsareimmutable'');end'),
('trigger:aircraft_propeller_catalog_models_require_approval', 'createtriggeraircraft_propeller_catalog_models_require_approvalbeforeinsertonaircraft_propeller_catalog_modelswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdecision_claimondecision_claim.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=decision_claim.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''propeller_model''anddecision_claim.evidence_claim_id=new.identity_evidence_claim_idanddecision_claim.evidence_rolein(''identity'',''specification'')andclaim.claim_kindin(''identity'',''specification'')andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))beginselectraise(abort,''propellercatalogmodelrequiresanapprovedprimary-sourceidentifier'');end'),
('trigger:aircraft_reference_avionics_building_insert', 'createtriggeraircraft_reference_avionics_building_insertbeforeinsertonaircraft_reference_avionicswhennotexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=new.aircraft_reference_configuration_version_idandversion.publication_state=''building'')ornotexists(select1fromavionics_modelsmodelwheremodel.id=new.avionics_model_idandmodel.catalog_status=''approved'')beginselectraise(abort,''referenceavionicsrequiresabuildingversionandapprovedproduct'');end'),
('trigger:aircraft_reference_avionics_immutable_delete', 'createtriggeraircraft_reference_avionics_immutable_deletebeforedeleteonaircraft_reference_avionicswhenexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=old.aircraft_reference_configuration_version_idandversion.publication_state<>''building'')beginselectraise(abort,''publishedreferenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_avionics_immutable_update', 'createtriggeraircraft_reference_avionics_immutable_updatebeforeupdateonaircraft_reference_avionicswhennot(new.id=old.idandnew.aircraft_reference_configuration_version_id=old.aircraft_reference_configuration_version_idandnew.avionics_model_idisnotold.avionics_model_idandnew.quantity=old.quantityandnew.equipment_role=old.equipment_roleandnew.evidence_claim_id=old.evidence_claim_idandnew.created_at=old.created_atandexists(select1fromavionics_catalog_authorized_consolidationsguardjoinavionics_modelssurvivoronsurvivor.id=guard.survivor_model_idjoinavionics_modelslegacyonlegacy.id=old.avionics_model_idwhereguard.duplicate_model_id=old.avionics_model_idandguard.survivor_model_id=new.avionics_model_id))beginselectraise(abort,''referenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_configurations_require_approval', 'createtriggeraircraft_reference_configurations_require_approvalbeforeinsertonaircraft_reference_configurationswhennotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''reference_configuration''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))or(new.aircraft_generation_idisnotnullandnotexists(select1fromaircraft_generation_designationslinkwherelink.aircraft_generation_id=new.aircraft_generation_idandlink.aircraft_designation_id=new.aircraft_designation_id))or(new.tier_package_idisnotnullandnotexists(select1fromaircraft_factory_packagespackagejoinaircraft_package_applicabilityapplicabilityonapplicability.aircraft_factory_package_id=package.idwherepackage.id=new.tier_package_idandpackage.package_kind=''trim_tier''andapplicability.aircraft_designation_id=new.aircraft_designation_idand(applicability.aircraft_generation_idisnullorapplicability.aircraft_generation_id=new.aircraft_generation_id)))beginselectraise(abort,''referenceconfigurationrequiresapprovedapplicableidentitydimensions'');end'),
('trigger:aircraft_reference_engines_building_insert', 'createtriggeraircraft_reference_engines_building_insertbeforeinsertonaircraft_reference_engineswhennotexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=new.aircraft_reference_configuration_version_idandversion.publication_state=''building'')ornotexists(select1fromaircraft_engine_catalog_modelsmodelwheremodel.id=new.aircraft_engine_catalog_model_idandmodel.catalog_status=''approved'')beginselectraise(abort,''referenceenginerequiresabuildingversionandapprovedcatalogmodel'');end'),
('trigger:aircraft_reference_engines_immutable_delete', 'createtriggeraircraft_reference_engines_immutable_deletebeforedeleteonaircraft_reference_engineswhenexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=old.aircraft_reference_configuration_version_idandversion.publication_state<>''building'')beginselectraise(abort,''publishedreferenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_engines_immutable_update', 'createtriggeraircraft_reference_engines_immutable_updatebeforeupdateonaircraft_reference_enginesbeginselectraise(abort,''referenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_fact_set_building_insert', 'createtriggeraircraft_reference_fact_set_building_insertbeforeinsertonaircraft_reference_fact_set_attestationswhennotexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=new.aircraft_reference_configuration_version_idandversion.publication_state=''building'')beginselectraise(abort,''referencefact-setattestationrequiresabuildingversion'');end'),
('trigger:aircraft_reference_fact_set_immutable_delete', 'createtriggeraircraft_reference_fact_set_immutable_deletebeforedeleteonaircraft_reference_fact_set_attestationswhenexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=old.aircraft_reference_configuration_version_idandversion.publication_state<>''building'')beginselectraise(abort,''publishedreferenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_fact_set_immutable_update', 'createtriggeraircraft_reference_fact_set_immutable_updatebeforeupdateonaircraft_reference_fact_set_attestationsbeginselectraise(abort,''referenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_features_building_insert', 'createtriggeraircraft_reference_features_building_insertbeforeinsertonaircraft_reference_featureswhennotexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=new.aircraft_reference_configuration_version_idandversion.publication_state=''building'')ornotexists(select1fromaircraft_feature_definitionsdefinitionwheredefinition.id=new.aircraft_feature_definition_idand((definition.value_type=''boolean''andnew.boolean_valueisnotnull)or(definition.value_type=''number''andnew.number_valueisnotnull)or(definition.value_type=''text''andnew.text_valueisnotnull)))beginselectraise(abort,''referencefeaturevaluedoesnotmatchitsdefinition'');end'),
('trigger:aircraft_reference_features_immutable_delete', 'createtriggeraircraft_reference_features_immutable_deletebeforedeleteonaircraft_reference_featureswhenexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=old.aircraft_reference_configuration_version_idandversion.publication_state<>''building'')beginselectraise(abort,''publishedreferenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_features_immutable_update', 'createtriggeraircraft_reference_features_immutable_updatebeforeupdateonaircraft_reference_featuresbeginselectraise(abort,''referenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_price_building_insert', 'createtriggeraircraft_reference_price_building_insertbeforeinsertonaircraft_reference_priceswhennotexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=new.aircraft_reference_configuration_version_idandversion.publication_state=''building'')beginselectraise(abort,''referencepricerequiresabuildingversion'');end'),
('trigger:aircraft_reference_price_immutable_delete', 'createtriggeraircraft_reference_price_immutable_deletebeforedeleteonaircraft_reference_priceswhenexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=old.aircraft_reference_configuration_version_idandversion.publication_state<>''building'')beginselectraise(abort,''publishedreferenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_price_immutable_update', 'createtriggeraircraft_reference_price_immutable_updatebeforeupdateonaircraft_reference_pricesbeginselectraise(abort,''referenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_propellers_building_insert', 'createtriggeraircraft_reference_propellers_building_insertbeforeinsertonaircraft_reference_propellerswhennotexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=new.aircraft_reference_configuration_version_idandversion.publication_state=''building'')ornotexists(select1fromaircraft_propeller_catalog_modelsmodelwheremodel.id=new.aircraft_propeller_catalog_model_idandmodel.catalog_status=''approved'')beginselectraise(abort,''referencepropellerrequiresabuildingversionandapprovedcatalogmodel'');end'),
('trigger:aircraft_reference_propellers_immutable_delete', 'createtriggeraircraft_reference_propellers_immutable_deletebeforedeleteonaircraft_reference_propellerswhenexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=old.aircraft_reference_configuration_version_idandversion.publication_state<>''building'')beginselectraise(abort,''publishedreferenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_propellers_immutable_update', 'createtriggeraircraft_reference_propellers_immutable_updatebeforeupdateonaircraft_reference_propellersbeginselectraise(abort,''referenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_scope_building_insert', 'createtriggeraircraft_reference_scope_building_insertbeforeinsertonaircraft_reference_applicability_scopeswhennotexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=new.aircraft_reference_configuration_version_idandversion.publication_state=''building'')beginselectraise(abort,''referenceprofilechildrenrequireabuildingversion'');end'),
('trigger:aircraft_reference_scope_canonical_insert', 'createtriggeraircraft_reference_scope_canonical_insertbeforeinsertonaircraft_reference_applicability_scopeswhennew.applies_to_all_serials=0and(new.serial_from_sort_key<>upper(new.serial_from_sort_key)ornew.serial_to_sort_key<>upper(new.serial_to_sort_key)ornew.serial_from_sort_keyglob''*[^a-f0-9]*''ornew.serial_to_sort_keyglob''*[^a-f0-9]*''orsubstr(new.serial_from_sort_key,1,2)<>''01''orsubstr(new.serial_to_sort_key,1,2)<>''01''orsubstr(new.serial_from_sort_key,-2)<>''00''orsubstr(new.serial_to_sort_key,-2)<>''00''ornew.serial_from_sort_keycollatebinary>new.serial_to_sort_keycollatebinaryornotexists(select1fromaircraft_serial_number_schemesschemewherescheme.id=new.aircraft_serial_number_scheme_idandscheme.normalization_version=''natural_alphanumeric_segments_v1'')or(new.serial_prefixisnotnulland(new.serial_prefix<>upper(new.serial_prefix)ornew.serial_prefixglob''*[^a-z0-9]*''orsubstr(new.serial_from_display,1,length(new.serial_prefix))<>new.serial_prefixorsubstr(new.serial_to_display,1,length(new.serial_prefix))<>new.serial_prefix)))beginselectraise(abort,''referenceserialapplicabilityrequirestheuniversalnatural-orderkey'');end'),
('trigger:aircraft_reference_scope_immutable_delete', 'createtriggeraircraft_reference_scope_immutable_deletebeforedeleteonaircraft_reference_applicability_scopeswhenexists(select1fromaircraft_reference_configuration_versionsversionwhereversion.id=old.aircraft_reference_configuration_version_idandversion.publication_state<>''building'')beginselectraise(abort,''publishedreferenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_scope_immutable_update', 'createtriggeraircraft_reference_scope_immutable_updatebeforeupdateonaircraft_reference_applicability_scopesbeginselectraise(abort,''referenceprofilefactsareimmutable'');end'),
('trigger:aircraft_reference_scope_key_recompute_insert', 'createtriggeraircraft_reference_scope_key_recompute_insertafterinsertonaircraft_reference_applicability_scopeswhenexists(select1fromaircraft_reference_serial_key_errorserrorwhereerror.scope_id=new.id)beginselectraise(abort,''referenceserialsortkeysmustberecomputedfromcanonicaldisplayvalues'');end'),
('trigger:aircraft_reference_versions_immutable', 'createtriggeraircraft_reference_versions_immutablebeforeupdateonaircraft_reference_configuration_versionswhenold.publication_statein(''published'',''superseded'')andnot(old.publication_state=''published''andnew.publication_state=''superseded''andnew.superseded_atisnotnullandnew.id=old.idandnew.aircraft_reference_configuration_id=old.aircraft_reference_configuration_idandnew.model_year=old.model_yearandnew.revision=old.revisionandnew.approval_decision_id=old.approval_decision_idandnew.published_at=old.published_atandnew.supersedes_version_idisold.supersedes_version_id)beginselectraise(abort,''publishedreferenceprofileversionsareimmutable'');end'),
('trigger:aircraft_reference_versions_publish', 'createtriggeraircraft_reference_versions_publishbeforeupdateofpublication_stateonaircraft_reference_configuration_versionswhennew.publication_state=''published''beginselectraise(abort,''onlyabuildingreferenceprofilecanbepublished'')whereold.publication_state<>''building'';selectraise(abort,''publishedreferenceprofilerequirespublished_at'')wherenew.published_atisnull;selectraise(abort,''publishedreferenceprofilerequiresapplicability'')wherenotexists(select1fromaircraft_reference_applicability_scopesscopewherescope.aircraft_reference_configuration_version_id=new.id);selectraise(abort,''publishedreferenceprofilerequirescompletefactoryfact-setattestations'')where4<>(selectcount(*)fromaircraft_reference_fact_set_attestationsattestationwhereattestation.aircraft_reference_configuration_version_id=new.id);selectraise(abort,''publishedreferenceprofilerequiresexactlyonedirectexact-model-yearfull-configurationequippedmsrpwithprimarypriceevidence'')where1<>(selectcount(*)fromaircraft_reference_pricespricejoincuration_evidence_claimsclaimonclaim.id=price.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwhereprice.aircraft_reference_configuration_version_id=new.idandprice.currency=''usd''andprice.price_kind=''equipped_msrp''andprice.evidence_kind=''direct_model_year''andprice.configuration_basis=''full_standard_configuration''andclaim.claim_kind=''price''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''));selectraise(abort,''publishedreferenceprofilerequiresapprovedenginecatalogmodels'')whereexists(select1fromaircraft_reference_enginesengineleftjoinaircraft_engine_catalog_modelsmodelonmodel.id=engine.aircraft_engine_catalog_model_idandmodel.catalog_status=''approved''whereengine.aircraft_reference_configuration_version_id=new.idandmodel.idisnull);selectraise(abort,''publishedreferenceprofilerequiresapprovedpropellercatalogmodels'')whereexists(select1fromaircraft_reference_propellerspropellerleftjoinaircraft_propeller_catalog_modelsmodelonmodel.id=propeller.aircraft_propeller_catalog_model_idandmodel.catalog_status=''approved''wherepropeller.aircraft_reference_configuration_version_id=new.idandmodel.idisnull);selectraise(abort,''publishedreferenceprofilefactsrequirevalidatedprimaryevidence'')whereexists(select1from(selectevidence_claim_id,''applicability''asevidence_domainfromaircraft_reference_applicability_scopeswhereaircraft_reference_configuration_version_id=new.idunionallselectevidence_claim_id,''price''fromaircraft_reference_priceswhereaircraft_reference_configuration_version_id=new.idunionallselectevidence_claim_id,''factory''fromaircraft_reference_avionicswhereaircraft_reference_configuration_version_id=new.idunionallselectevidence_claim_id,''factory''fromaircraft_reference_engineswhereaircraft_reference_configuration_version_id=new.idunionallselectevidence_claim_id,''factory''fromaircraft_reference_propellerswhereaircraft_reference_configuration_version_id=new.idunionallselectevidence_claim_id,''factory''fromaircraft_reference_featureswhereaircraft_reference_configuration_version_id=new.idunionallselectevidence_claim_id,''factory''fromaircraft_reference_fact_set_attestationswhereaircraft_reference_configuration_version_id=new.id)factjoincuration_evidence_claimsclaimonclaim.id=fact.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwhereclaim.validation_status<>''validated''orsource.source_tiernotin(''manufacturer_primary'',''regulator_primary'')or(fact.evidence_domain=''applicability''andclaim.claim_kind<>''applicability'')or(fact.evidence_domain=''price''andclaim.claim_kind<>''price'')or(fact.evidence_domain=''factory''andclaim.claim_kindnotin(''standard_equipment'',''package_composition'',''specification'')));selectraise(abort,''referenceprofilecontainsoverlappingapplicabilityscopes'')whereexists(select1fromaircraft_reference_applicability_scopesleft_scopejoinaircraft_reference_applicability_scopesright_scopeonright_scope.aircraft_reference_configuration_version_id=left_scope.aircraft_reference_configuration_version_idandright_scope.id>left_scope.idandright_scope.aircraft_market_id=left_scope.aircraft_market_idwhereleft_scope.aircraft_reference_configuration_version_id=new.idand(left_scope.applies_to_all_serials=1orright_scope.applies_to_all_serials=1or(left_scope.serial_from_sort_keycollatebinary<=right_scope.serial_to_sort_keycollatebinaryandright_scope.serial_from_sort_keycollatebinary<=left_scope.serial_to_sort_keycollatebinary)));selectraise(abort,''publishedreferenceprofileapplicabilityoverlapsanexistingversion'')whereexists(select1fromaircraft_reference_applicability_scopescandidatejoinaircraft_marketscandidate_marketoncandidate_market.id=candidate.aircraft_market_idjoinaircraft_reference_applicability_scopesexistingonexisting.aircraft_market_id=candidate.aircraft_market_idorcandidate_market.code=''global''orexists(select1fromaircraft_marketsexisting_marketwhereexisting_market.id=existing.aircraft_market_idandexisting_market.code=''global'')joinaircraft_reference_configuration_versionsexisting_versiononexisting_version.id=existing.aircraft_reference_configuration_version_idwherecandidate.aircraft_reference_configuration_version_id=new.idandexisting_version.id<>new.idandexisting_version.aircraft_reference_configuration_id=new.aircraft_reference_configuration_idandexisting_version.model_year=new.model_yearandexisting_version.publication_state=''published''and(candidate.applies_to_all_serials=1orexisting.applies_to_all_serials=1or(candidate.serial_from_sort_keycollatebinary<=existing.serial_to_sort_keycollatebinaryandexisting.serial_from_sort_keycollatebinary<=candidate.serial_to_sort_keycollatebinary)));end'),
('trigger:aircraft_reference_versions_require_approval', 'createtriggeraircraft_reference_versions_require_approvalbeforeinsertonaircraft_reference_configuration_versionswhennew.publication_state<>''building''ornotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''reference_profile''andclaim.validation_status=''validated''andsource.source_tierin(''manufacturer_primary'',''regulator_primary''))or(new.revision=1)<>(new.supersedes_version_idisnull)or(new.supersedes_version_idisnotnullandnotexists(select1fromaircraft_reference_configuration_versionspreviouswhereprevious.id=new.supersedes_version_idandprevious.aircraft_reference_configuration_id=new.aircraft_reference_configuration_idandprevious.model_year=new.model_yearandprevious.revision=new.revision-1andprevious.publication_state=''published''))beginselectraise(abort,''referenceprofilerequiresbuildingstate,approvedevidence,anditsexactpredecessor'');end'),
('trigger:aircraft_serial_schemes_preserve_ordering', 'createtriggeraircraft_serial_schemes_preserve_orderingbeforeupdateofnormalization_versiononaircraft_serial_number_schemeswhennew.normalization_version<>''natural_alphanumeric_segments_v1''beginselectraise(abort,''serialschemeorderingversionisimmutable'');end'),
('trigger:aircraft_serial_schemes_require_approval', 'createtriggeraircraft_serial_schemes_require_approvalbeforeinsertonaircraft_serial_number_schemeswhennew.normalization_version<>''natural_alphanumeric_segments_v1''ornotexists(select1fromaircraft_identity_decisionsdecisionjoinaircraft_identity_decision_claimsdcondc.decision_id=decision.idjoincuration_evidence_claimsclaimonclaim.id=dc.evidence_claim_idwheredecision.id=new.approval_decision_idanddecision.decision_status=''approved''anddecision.decision_action=''approve_new''anddecision.entity_kind=''serial_scheme''andclaim.validation_status=''validated'')beginselectraise(abort,''serialschemerequirestheuniversalorderingandanapprovedevidence-backeddecision'');end'),
('trigger:aircraft_valuation_projection_immutable_delete', 'createtriggeraircraft_valuation_projection_immutable_deletebeforedeleteonaircraft_valuation_compatibility_projectionsbeginselectraise(abort,''aircraftcompatibilityprojectionsareimmutable'');end'),
('trigger:aircraft_valuation_projection_immutable_update', 'createtriggeraircraft_valuation_projection_immutable_updatebeforeupdateonaircraft_valuation_compatibility_projectionsbeginselectraise(abort,''aircraftcompatibilityprojectionsareimmutable'');end'),
('trigger:aircraft_valuation_projection_validate_insert', 'createtriggeraircraft_valuation_projection_validate_insertbeforeinsertonaircraft_valuation_compatibility_projectionswhennotexists(select1fromaircraft_valuation_projection_transitionstransitionjoinaircraft_sale_listing_identity_assignmentsassignmentonassignment.id=transition.identity_assignment_idandassignment.aircraft_sale_listing_id=transition.aircraft_sale_listing_idjoinaircraft_makesmakeonmake.id=assignment.aircraft_make_idjoinaircraft_model_familiesfamilyonfamily.id=assignment.aircraft_model_family_idandfamily.aircraft_make_id=make.idjoinaircraft_designationsdesignationondesignation.id=assignment.aircraft_designation_idanddesignation.aircraft_model_family_id=family.idleftjoinaircraft_generationsgenerationongeneration.id=assignment.aircraft_generation_idandgeneration.aircraft_model_family_id=family.idleftjoinaircraft_factory_packagespackageonpackage.id=assignment.aircraft_factory_package_idandpackage.aircraft_model_family_id=family.idjoinaircraft_model_variantslegacy_variantonlegacy_variant.id=new.aircraft_model_variant_idjoinaircraft_modelslegacy_modelonlegacy_model.id=legacy_variant.aircraft_model_idjoinaircraft_manufacturerslegacy_manufactureronlegacy_manufacturer.id=legacy_model.aircraft_manufacturer_idwhereassignment.aircraft_make_id=new.aircraft_make_idandassignment.aircraft_model_family_id=new.aircraft_model_family_idandassignment.aircraft_designation_id=new.aircraft_designation_idandassignment.aircraft_generation_idisnew.aircraft_generation_idandassignment.aircraft_factory_package_idisnew.aircraft_factory_package_idandassignment.aircraft_sale_listing_id=new.created_from_aircraft_sale_listing_idandassignment.id=new.created_from_identity_assignment_idandassignment.identity_decision_id=new.identity_decision_idandassignment.identity_evidence_claim_id=new.identity_evidence_claim_idandassignment.faa_registry_snapshot_id=new.faa_registry_snapshot_idandassignment.faa_n_number=new.faa_n_numberandassignment.faa_source_record_sha256=new.faa_source_record_sha256andlegacy_manufacturer.name=make.nameandlegacy_manufacturer.normalized_name=''__aircost_projection_make_''||make.id||''__''andlegacy_model.name=family.nameandlegacy_model.normalized_name=''__aircost_projection_family_''||family.id||''__''andlegacy_variant.name=designation.official_designation||casewhengeneration.idisnullthen''''else''/''||generation.nameend||casewhenpackage.idisnullthen''''else''/''||package.nameendandlegacy_variant.normalized_name=''__aircost_projection_identity_''||designation.id||''_''||coalesce(generation.id,0)||''_''||coalesce(package.id,0)||''__''and(assignment.aircraft_generation_idisnullorexists(select1fromaircraft_generation_designationsapplicabilitywhereapplicability.aircraft_generation_id=assignment.aircraft_generation_idandapplicability.aircraft_designation_id=assignment.aircraft_designation_id))and(assignment.aircraft_factory_package_idisnullorexists(select1fromaircraft_package_applicabilityapplicabilitywhereapplicability.aircraft_factory_package_id=assignment.aircraft_factory_package_idandapplicability.aircraft_designation_id=assignment.aircraft_designation_idand(applicability.aircraft_generation_idisnullorapplicability.aircraft_generation_idisassignment.aircraft_generation_id)))andnotexists(select1fromaircraft_sale_listingschildwherechild.aircraft_model_variant_id=legacy_variant.id)andnotexists(select1fromrental_aircraft_offeringschildwherechild.aircraft_model_variant_id=legacy_variant.id))beginselectraise(abort,''aircraftcompatibilityprojectionrequirestheactivecommand,exactcopiedassignmentprovenance,anditsfreshreservedhierarchy'');end'),
('trigger:assigned_aircraft_designation_immutable_delete', 'createtriggerassigned_aircraft_designation_immutable_deletebeforedeleteonaircraft_designationswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_designation_id=old.id)beginselectraise(abort,''assignedaircraftdesignationsareimmutable'');end'),
('trigger:assigned_aircraft_designation_immutable_update', 'createtriggerassigned_aircraft_designation_immutable_updatebeforeupdateonaircraft_designationswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_designation_id=old.id)beginselectraise(abort,''assignedaircraftdesignationsareimmutable'');end'),
('trigger:assigned_aircraft_family_immutable_delete', 'createtriggerassigned_aircraft_family_immutable_deletebeforedeleteonaircraft_model_familieswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_model_family_id=old.id)beginselectraise(abort,''assignedaircraftmodelfamiliesareimmutable'');end'),
('trigger:assigned_aircraft_family_immutable_update', 'createtriggerassigned_aircraft_family_immutable_updatebeforeupdateonaircraft_model_familieswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_model_family_id=old.id)beginselectraise(abort,''assignedaircraftmodelfamiliesareimmutable'');end'),
('trigger:assigned_aircraft_generation_immutable_delete', 'createtriggerassigned_aircraft_generation_immutable_deletebeforedeleteonaircraft_generationswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_generation_id=old.id)beginselectraise(abort,''assignedaircraftgenerationsareimmutable'');end'),
('trigger:assigned_aircraft_generation_immutable_update', 'createtriggerassigned_aircraft_generation_immutable_updatebeforeupdateonaircraft_generationswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_generation_id=old.id)beginselectraise(abort,''assignedaircraftgenerationsareimmutable'');end'),
('trigger:assigned_aircraft_make_immutable_delete', 'createtriggerassigned_aircraft_make_immutable_deletebeforedeleteonaircraft_makeswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_make_id=old.id)beginselectraise(abort,''assignedaircraftmakesareimmutable'');end'),
('trigger:assigned_aircraft_make_immutable_update', 'createtriggerassigned_aircraft_make_immutable_updatebeforeupdateonaircraft_makeswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_make_id=old.id)beginselectraise(abort,''assignedaircraftmakesareimmutable'');end'),
('trigger:assigned_aircraft_package_immutable_delete', 'createtriggerassigned_aircraft_package_immutable_deletebeforedeleteonaircraft_factory_packageswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_factory_package_id=old.id)beginselectraise(abort,''assignedaircraftfactorypackagesareimmutable'');end'),
('trigger:assigned_aircraft_package_immutable_update', 'createtriggerassigned_aircraft_package_immutable_updatebeforeupdateonaircraft_factory_packageswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_factory_package_id=old.id)beginselectraise(abort,''assignedaircraftfactorypackagesareimmutable'');end'),
('trigger:assigned_generation_designation_immutable_delete', 'createtriggerassigned_generation_designation_immutable_deletebeforedeleteonaircraft_generation_designationswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_generation_id=old.aircraft_generation_idandassignment.aircraft_designation_id=old.aircraft_designation_id)beginselectraise(abort,''assignedgeneration/designationapplicabilityisimmutable'');end'),
('trigger:assigned_generation_designation_immutable_update', 'createtriggerassigned_generation_designation_immutable_updatebeforeupdateonaircraft_generation_designationswhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_generation_id=old.aircraft_generation_idandassignment.aircraft_designation_id=old.aircraft_designation_id)beginselectraise(abort,''assignedgeneration/designationapplicabilityisimmutable'');end'),
('trigger:assigned_generation_dimension_requires_resolution', 'createtriggerassigned_generation_dimension_requires_resolutionbeforeinsertonaircraft_generation_designationswhenexists(select1fromaircraft_sale_listing_current_identity_assignmentscurrent_assignmentjoinaircraft_sale_listing_identity_assignmentsassignmentonassignment.id=current_assignment.identity_assignment_idandassignment.aircraft_sale_listing_id=current_assignment.aircraft_sale_listing_idjoinaircraft_sale_listingslistingonlisting.id=current_assignment.aircraft_sale_listing_idwherelisting.ingestion_state=''ready''andassignment.aircraft_designation_id=new.aircraft_designation_idandassignment.aircraft_generation_idisnull)beginselectraise(abort,''addingagenerationdimensionrequiresresolvingaffectedreadylistingassignmentsfirst'');end'),
('trigger:assigned_package_applicability_immutable_delete', 'createtriggerassigned_package_applicability_immutable_deletebeforedeleteonaircraft_package_applicabilitywhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_factory_package_id=old.aircraft_factory_package_idandassignment.aircraft_designation_id=old.aircraft_designation_idand(old.aircraft_generation_idisnullorassignment.aircraft_generation_id=old.aircraft_generation_id))beginselectraise(abort,''assignedpackageapplicabilityisimmutable'');end'),
('trigger:assigned_package_applicability_immutable_update', 'createtriggerassigned_package_applicability_immutable_updatebeforeupdateonaircraft_package_applicabilitywhenexists(select1fromaircraft_sale_listing_identity_assignmentsassignmentwhereassignment.aircraft_factory_package_id=old.aircraft_factory_package_idandassignment.aircraft_designation_id=old.aircraft_designation_idand(old.aircraft_generation_idisnullorassignment.aircraft_generation_id=old.aircraft_generation_id))beginselectraise(abort,''assignedpackageapplicabilityisimmutable'');end'),
('trigger:assigned_trim_tier_dimension_requires_resolution', 'createtriggerassigned_trim_tier_dimension_requires_resolutionbeforeinsertonaircraft_package_applicabilitywhenexists(select1fromaircraft_factory_packagespackagecrossjoinaircraft_sale_listing_current_identity_assignmentscurrent_assignmentjoinaircraft_sale_listing_identity_assignmentsassignmentonassignment.id=current_assignment.identity_assignment_idandassignment.aircraft_sale_listing_id=current_assignment.aircraft_sale_listing_idjoinaircraft_sale_listingslistingonlisting.id=current_assignment.aircraft_sale_listing_idwherepackage.id=new.aircraft_factory_package_idandpackage.package_kind=''trim_tier''andlisting.ingestion_state=''ready''andassignment.aircraft_designation_id=new.aircraft_designation_idandassignment.aircraft_factory_package_idisnulland(new.aircraft_generation_idisnullorassignment.aircraft_generation_id=new.aircraft_generation_id)and(new.valid_from_model_yearisnullornew.valid_from_model_year<=listing.model_year)and(new.valid_to_model_yearisnullornew.valid_to_model_year>=listing.model_year))beginselectraise(abort,''addingatrim-tierdimensionrequiresresolvingaffectedreadylistingassignmentsfirst'');end'),
('trigger:avionics_models_approved_delete_guard', 'createtriggeravionics_models_approved_delete_guardbeforedeleteonavionics_modelswhenold.catalog_status=''approved''andnotexists(select1fromavionics_catalog_authorized_consolidationsauthorizationjoinavionics_modelssurvivoronsurvivor.id=authorization.survivor_model_idwhereauthorization.duplicate_model_id=old.idandsurvivor.catalog_status=''approved'')beginselectraise(abort,''approvedavionicsproductdeletionrequiresexactconsolidationauthorization'');end'),
('trigger:avionics_models_approved_identity_immutable', 'createtriggeravionics_models_approved_identity_immutablebeforeupdateonavionics_modelswhenold.catalog_status=''approved''and(new.catalog_statusisnotold.catalog_statusornew.avionics_manufacturer_idisnotold.avionics_manufacturer_idornew.nameisnotold.nameornew.normalized_nameisnotold.normalized_nameornew.manufacturer_identifier_kindisnotold.manufacturer_identifier_kindornew.manufacturer_identifierisnotold.manufacturer_identifierornew.normalized_manufacturer_identifierisnotold.normalized_manufacturer_identifier)beginselectraise(abort,''approvedavionicsproductcannotbedemotedorrewritecanonicalidentity'');end'),
('trigger:avionics_models_approved_types_insert', 'createtriggeravionics_models_approved_types_insertbeforeinsertonavionics_modelswhennew.catalog_status=''approved''beginselectraise(abort,''avionicsapprovalmustbestagedfromanunreviewedproduct'');end'),
('trigger:avionics_models_approved_types_update', 'createtriggeravionics_models_approved_types_updatebeforeupdateofcatalog_statusonavionics_modelswhennew.catalog_status=''approved''andnotexists(select1fromavionics_model_typesmembershipwheremembership.avionics_model_id=new.id)beginselectraise(abort,''approvedavionicsmodelrequiresatleastonetype'');end'),
('trigger:avionics_models_canonical_identity_sync_update', 'createtriggeravionics_models_canonical_identity_sync_updateafterupdateofcatalog_status,avionics_manufacturer_id,normalized_name,normalized_manufacturer_identifieronavionics_modelswhennew.catalog_status=''approved''begininsertintoavionics_approved_product_identities(avionics_model_id,avionics_manufacturer_identity_id,canonical_product_key,manufacturer_identifier_kind,canonical_identifier_key)selectnew.id,manufacturer_identity.avionics_manufacturer_identity_id,lower(replace(replace(replace(replace(replace(trim(new.normalized_name),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'','''')),new.manufacturer_identifier_kind,lower(replace(replace(replace(replace(replace(trim(new.normalized_manufacturer_identifier),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'',''''))fromavionics_manufacturer_effective_membershipsmanufacturer_identitywheremanufacturer_identity.avionics_manufacturer_id=new.avionics_manufacturer_idonconflict(avionics_model_id)doupdatesetavionics_manufacturer_identity_id=excluded.avionics_manufacturer_identity_id,canonical_product_key=excluded.canonical_product_key,manufacturer_identifier_kind=excluded.manufacturer_identifier_kind,canonical_identifier_key=excluded.canonical_identifier_key,updated_at=current_timestamp;end'),
('trigger:avionics_models_canonical_identity_validate_update', 'createtriggeravionics_models_canonical_identity_validate_updatebeforeupdateofcatalog_status,avionics_manufacturer_id,normalized_name,normalized_manufacturer_identifieronavionics_modelswhennew.catalog_status=''approved''and(notexists(select1fromavionics_manufacturer_effective_membershipsmanufacturer_identitywheremanufacturer_identity.avionics_manufacturer_id=new.avionics_manufacturer_id)orlength(lower(replace(replace(replace(replace(replace(trim(new.normalized_name),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'','''')))=0orlength(lower(replace(replace(replace(replace(replace(trim(new.normalized_manufacturer_identifier),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'','''')))=0ornew.nameglob''*[^a-za-z0-9./_-]*''ornew.normalized_nameglob''*[^a-za-z0-9./_-]*''orlower(replace(replace(replace(replace(replace(trim(new.name),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'',''''))<>lower(replace(replace(replace(replace(replace(trim(new.normalized_name),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'',''''))ornew.manufacturer_identifierisnullornew.manufacturer_identifierglob''*[^a-za-z0-9./_-]*''ornew.normalized_manufacturer_identifierglob''*[^a-za-z0-9./_-]*''orlower(replace(replace(replace(replace(replace(trim(new.manufacturer_identifier),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'',''''))<>lower(replace(replace(replace(replace(replace(trim(new.normalized_manufacturer_identifier),'''',''''),''-'',''''),''/'',''''),''.'',''''),''_'','''')))beginselectraise(abort,''approvedavionicsproductrequiresdeterministiccanonicalidentitykeys'');end'),
('trigger:avionics_models_consolidation_identity_immutable', 'createtriggeravionics_models_consolidation_identity_immutablebeforeupdateofcatalog_status,avionics_manufacturer_id,name,normalized_name,manufacturer_identifier_kind,manufacturer_identifier,normalized_manufacturer_identifieronavionics_modelswhenexists(select1fromavionics_catalog_consolidation_guardguardwhereguard.duplicate_model_id=old.idorguard.survivor_model_id=old.idunionallselect1fromavionics_catalog_grounded_consolidation_guardgrounded_guardwheregrounded_guard.duplicate_model_id=old.idorgrounded_guard.survivor_model_id=old.idunionallselect1fromavionics_catalog_human_consolidation_guardguardwhereguard.duplicate_model_id=old.idorguard.survivor_model_id=old.id)beginselectraise(abort,''guardedavionicsconsolidationidentitiesareimmutable'');end'),
('trigger:avionics_models_referenced_status_update', 'createtriggeravionics_models_referenced_status_updatebeforeupdateofcatalog_statusonavionics_modelswhennew.catalog_status<>''approved''and(exists(select1fromaircraft_sale_listing_avionicslisting_linkwherelisting_link.avionics_model_id=old.idorlisting_link.replaces_avionics_model_id=old.id)orexists(select1fromavionics_suite_componentssuite_linkwheresuite_link.suite_model_id=old.idorsuite_link.component_model_id=old.id)orexists(select1fromaircraft_reference_avionicsreference_linkwherereference_link.avionics_model_id=old.id))beginselectraise(abort,''referencedavionicscatalogentrycannotbeunapproved'');end'),
('trigger:compatibility_projected_designation_immutable_delete', 'createtriggercompatibility_projected_designation_immutable_deletebeforedeleteonaircraft_designationswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_designation_id=old.id)beginselectraise(abort,''compatibility-projectedcanonicalaircraftdesignationsareimmutable'');end'),
('trigger:compatibility_projected_designation_immutable_update', 'createtriggercompatibility_projected_designation_immutable_updatebeforeupdateonaircraft_designationswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_designation_id=old.id)beginselectraise(abort,''compatibility-projectedcanonicalaircraftdesignationsareimmutable'');end'),
('trigger:compatibility_projected_family_immutable_delete', 'createtriggercompatibility_projected_family_immutable_deletebeforedeleteonaircraft_model_familieswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_model_family_id=old.id)beginselectraise(abort,''compatibility-projectedcanonicalaircraftfamiliesareimmutable'');end'),
('trigger:compatibility_projected_family_immutable_update', 'createtriggercompatibility_projected_family_immutable_updatebeforeupdateonaircraft_model_familieswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_model_family_id=old.id)beginselectraise(abort,''compatibility-projectedcanonicalaircraftfamiliesareimmutable'');end'),
('trigger:compatibility_projected_generation_immutable_delete', 'createtriggercompatibility_projected_generation_immutable_deletebeforedeleteonaircraft_generationswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_generation_id=old.id)beginselectraise(abort,''compatibility-projectedaircraftgenerationsareimmutable'');end'),
('trigger:compatibility_projected_generation_immutable_update', 'createtriggercompatibility_projected_generation_immutable_updatebeforeupdateonaircraft_generationswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_generation_id=old.id)beginselectraise(abort,''compatibility-projectedaircraftgenerationsareimmutable'');end'),
('trigger:compatibility_projected_generation_link_immutable_delete', 'createtriggercompatibility_projected_generation_link_immutable_deletebeforedeleteonaircraft_generation_designationswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_generation_id=old.aircraft_generation_idandprojection.aircraft_designation_id=old.aircraft_designation_id)beginselectraise(abort,''compatibility-projectedgenerationapplicabilityisimmutable'');end'),
('trigger:compatibility_projected_generation_link_immutable_update', 'createtriggercompatibility_projected_generation_link_immutable_updatebeforeupdateonaircraft_generation_designationswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_generation_id=old.aircraft_generation_idandprojection.aircraft_designation_id=old.aircraft_designation_id)beginselectraise(abort,''compatibility-projectedgenerationapplicabilityisimmutable'');end'),
('trigger:compatibility_projected_make_immutable_delete', 'createtriggercompatibility_projected_make_immutable_deletebeforedeleteonaircraft_makeswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_make_id=old.id)beginselectraise(abort,''compatibility-projectedcanonicalaircraftmakesareimmutable'');end'),
('trigger:compatibility_projected_make_immutable_update', 'createtriggercompatibility_projected_make_immutable_updatebeforeupdateonaircraft_makeswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_make_id=old.id)beginselectraise(abort,''compatibility-projectedcanonicalaircraftmakesareimmutable'');end'),
('trigger:compatibility_projected_package_immutable_delete', 'createtriggercompatibility_projected_package_immutable_deletebeforedeleteonaircraft_factory_packageswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_factory_package_id=old.id)beginselectraise(abort,''compatibility-projectedaircraftpackagesareimmutable'');end'),
('trigger:compatibility_projected_package_immutable_update', 'createtriggercompatibility_projected_package_immutable_updatebeforeupdateonaircraft_factory_packageswhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_factory_package_id=old.id)beginselectraise(abort,''compatibility-projectedaircraftpackagesareimmutable'');end'),
('trigger:compatibility_projected_package_link_immutable_delete', 'createtriggercompatibility_projected_package_link_immutable_deletebeforedeleteonaircraft_package_applicabilitywhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_factory_package_id=old.aircraft_factory_package_idandprojection.aircraft_designation_id=old.aircraft_designation_idand(old.aircraft_generation_idisnullorprojection.aircraft_generation_id=old.aircraft_generation_id))beginselectraise(abort,''compatibility-projectedpackageapplicabilityisimmutable'');end'),
('trigger:compatibility_projected_package_link_immutable_update', 'createtriggercompatibility_projected_package_link_immutable_updatebeforeupdateonaircraft_package_applicabilitywhenexists(select1fromaircraft_valuation_compatibility_projectionsprojectionwhereprojection.aircraft_factory_package_id=old.aircraft_factory_package_idandprojection.aircraft_designation_id=old.aircraft_designation_idand(old.aircraft_generation_idisnullorprojection.aircraft_generation_id=old.aircraft_generation_id))beginselectraise(abort,''compatibility-projectedpackageapplicabilityisimmutable'');end'),
('trigger:listing_avionics_authorizations_invalidate_capture_delete', 'createtriggerlisting_avionics_authorizations_invalidate_capture_deleteafterdeleteonplugin_submissionsbegindeletefromaircraft_sale_listing_avionics_authorizationswhereevidence_capture_sha256=old.rendered_html_sha256andexists(select1fromaircraft_sale_listing_avionicslinkwherelink.id=aircraft_sale_listing_avionics_authorizations.listing_link_idandlink.aircraft_sale_listing_id=old.canonical_listing_idandlength(trim(coalesce(link.source_notes,'''')))>0andinstr(old.rendered_html,link.source_notes)>0andnotexists(select1fromplugin_submissionsretained_capturewhereretained_capture.canonical_listing_id=link.aircraft_sale_listing_idandretained_capture.rendered_html_sha256=aircraft_sale_listing_avionics_authorizations.evidence_capture_sha256andinstr(retained_capture.rendered_html,link.source_notes)>0));end'),
('trigger:listing_avionics_authorizations_invalidate_capture_update', 'createtriggerlisting_avionics_authorizations_invalidate_capture_updateafterupdateofcanonical_listing_id,rendered_html,rendered_html_sha256onplugin_submissionsbegindeletefromaircraft_sale_listing_avionics_authorizationswhereevidence_capture_sha256=old.rendered_html_sha256andexists(select1fromaircraft_sale_listing_avionicslinkwherelink.id=aircraft_sale_listing_avionics_authorizations.listing_link_idandlink.aircraft_sale_listing_id=old.canonical_listing_idandlength(trim(coalesce(link.source_notes,'''')))>0andinstr(old.rendered_html,link.source_notes)>0andnotexists(select1fromplugin_submissionsretained_capturewhereretained_capture.canonical_listing_id=link.aircraft_sale_listing_idandretained_capture.rendered_html_sha256=aircraft_sale_listing_avionics_authorizations.evidence_capture_sha256andinstr(retained_capture.rendered_html,link.source_notes)>0));end'),
('trigger:listing_avionics_authorizations_invalidate_model_proof_update', 'createtriggerlisting_avionics_authorizations_invalidate_model_proof_updateafterupdateofavionics_manufacturer_id,name,normalized_name,catalog_status,manufacturer_identifier_kind,manufacturer_identifier,normalized_manufacturer_identifier,identity_source_url,identity_source_title,identity_evidence_textonavionics_modelsbegindeletefromaircraft_sale_listing_avionics_authorizationswhereauthorization_kind=''same_case_grounded''andavionics_model_id=old.id;end'),
('trigger:official_dollar_normalization_immutable_delete', 'createtriggerofficial_dollar_normalization_immutable_deletebeforedeleteonofficial_dollar_normalization_factsbeginselectraise(abort,''officialdollarnormalizationfactsareimmutable'');end'),
('trigger:official_dollar_normalization_immutable_update', 'createtriggerofficial_dollar_normalization_immutable_updatebeforeupdateonofficial_dollar_normalization_factsbeginselectraise(abort,''officialdollarnormalizationfactsareimmutable'');end'),
('trigger:official_dollar_normalization_require_evidence', 'createtriggerofficial_dollar_normalization_require_evidencebeforeinsertonofficial_dollar_normalization_factswhennotexists(select1fromcuration_evidence_claimsclaimjoincuration_evidence_sourcessourceonsource.id=claim.evidence_source_idwhereclaim.id=new.evidence_claim_idandclaim.validation_status=''validated''andclaim.claim_kindin(''price'',''specification'')andsource.source_tier=''regulator_primary'')beginselectraise(abort,''dollarnormalizationrequiresvalidatedofficialregulatorevidence'');end'),
('trigger:plugin_submissions_replay_checkpoint_immutable', 'createtriggerplugin_submissions_replay_checkpoint_immutablebeforeupdateonplugin_submissionswhen(exists(select1fromlisting_replay_run_itemsitemwhereitem.plugin_submission_id=old.idanditem.extraction_state=''succeeded'')orexists(select1fromplugin_submission_materialization_receiptsreceiptwherereceipt.plugin_submission_id=old.id))and(not(new.idisold.id)ornot(new.user_idisold.user_id)ornot(new.plugin_install_idisold.plugin_install_id)ornot(new.source_urlisold.source_url)ornot(new.submitted_atisold.submitted_at)ornot(new.rendered_htmlisold.rendered_html)ornot(new.rendered_html_sha256isold.rendered_html_sha256)ornot(new.signature_base64isold.signature_base64)ornot(new.extracted_listing_jsonisold.extracted_listing_json)ornot(new.extraction_errorisold.extraction_error)ornot(new.canonical_listing_idisold.canonical_listing_idor(old.canonical_listing_idisnullandnew.canonical_listing_idisnotnullandnotexists(select1fromplugin_submission_materialization_receiptsreceiptwherereceipt.plugin_submission_id=old.id)andexists(select1fromaircraft_sale_listingslistingwherelisting.id=new.canonical_listing_idandlisting.created_by_user_id=old.user_idandlisting.source_url=old.source_url))))beginselectraise(abort,''replaycheckpointcaptureisimmutable'');end'),
('view:aircraft_reference_serial_key_errors', 'createviewaircraft_reference_serial_key_errorsaswithrecursivebounds(scope_id,bound_name,serial_value,stored_key)as(selectid,''from'',serial_from_display,serial_from_sort_keyfromaircraft_reference_applicability_scopeswhereapplies_to_all_serials=0unionallselectid,''to'',serial_to_display,serial_to_sort_keyfromaircraft_reference_applicability_scopeswhereapplies_to_all_serials=0),state(scope_id,bound_name,serial_value,stored_key,position,segment,alpha_hex,numeric_segment,encoded)as(selectscope_id,bound_name,serial_value,stored_key,2,substr(serial_value,1,1),casewhensubstr(serial_value,1,1)glob''[0-9]''then''''elseprintf(''%02x'',instr(''abcdefghijklmnopqrstuvwxyz'',substr(serial_value,1,1)))end,substr(serial_value,1,1)glob''[0-9]'',''01''fromboundsunionallselectscope_id,bound_name,serial_value,stored_key,position+1,casewhen(substr(serial_value,position,1)glob''[0-9]'')=numeric_segmentthensegment||substr(serial_value,position,1)elsesubstr(serial_value,position,1)end,casewhen(substr(serial_value,position,1)glob''[0-9]'')=numeric_segmentthenalpha_hex||casewhennumeric_segmentthen''''elseprintf(''%02x'',instr(''abcdefghijklmnopqrstuvwxyz'',substr(serial_value,position,1)))endelsecasewhensubstr(serial_value,position,1)glob''[0-9]''then''''elseprintf(''%02x'',instr(''abcdefghijklmnopqrstuvwxyz'',substr(serial_value,position,1)))endend,substr(serial_value,position,1)glob''[0-9]'',casewhen(substr(serial_value,position,1)glob''[0-9]'')=numeric_segmentthenencodedelseencoded||casewhennumeric_segmentthen''20''||printf(''%08x'',length(casewhentrim(segment,''0'')=''''then''0''elseltrim(segment,''0'')end))||casewhentrim(segment,''0'')=''''then''0''elseltrim(segment,''0'')end||printf(''%08x'',length(segment))||segmentelse''10''||alpha_hex||''00''endendfromstatewhereposition<=length(serial_value)),expected(scope_id,bound_name,expected_key)as(selectscope_id,bound_name,encoded||casewhennumeric_segmentthen''20''||printf(''%08x'',length(casewhentrim(segment,''0'')=''''then''0''elseltrim(segment,''0'')end))||casewhentrim(segment,''0'')=''''then''0''elseltrim(segment,''0'')end||printf(''%08x'',length(segment))||segmentelse''10''||alpha_hex||''00''end||''00''fromstatewhereposition=length(serial_value)+1)selectbounds.scope_id,bounds.bound_name,bounds.serial_value,bounds.stored_key,expected.expected_keyfromboundsleftjoinexpectedonexpected.scope_id=bounds.scope_idandexpected.bound_name=bounds.bound_namewherebounds.serial_valueisnullorbounds.serial_value=''''orbounds.serial_value<>upper(bounds.serial_value)orbounds.serial_valueglob''*[^a-z0-9]*''orexpected.expected_keyisnullorbounds.stored_keyisnotexpected.expected_key');


-- A marker asserts the exact complete cutover, not permission for later
-- CREATE IF NOT EXISTS statements to heal damaged or additive owned objects.
CREATE TEMP TABLE reference_catalog_schema_owned_objects AS
WITH
protected_relations(name) AS (
  VALUES
    ('plugin_submissions'),
    ('avionics_models'),
    ('aircraft_engine_catalog_models'),
    ('aircraft_propeller_catalog_models'),
    ('aircraft_makes'),
    ('aircraft_model_families'),
    ('aircraft_designations'),
    ('aircraft_make_aliases'),
    ('aircraft_family_aliases'),
    ('aircraft_designation_aliases'),
    ('aircraft_designation_identifiers'),
    ('aircraft_generations'),
    ('aircraft_generation_designations'),
    ('aircraft_factory_packages'),
    ('aircraft_package_applicability'),
    ('aircraft_reference_configurations'),
    ('aircraft_serial_number_schemes'),
    ('aircraft_feature_definitions'),
    ('aircraft_reference_configuration_versions'),
    ('aircraft_reference_applicability_scopes'),
    ('aircraft_reference_prices'),
    ('aircraft_reference_avionics'),
    ('aircraft_reference_engines'),
    ('aircraft_reference_propellers'),
    ('aircraft_reference_features'),
    ('aircraft_reference_fact_set_attestations'),
    ('official_dollar_normalization_facts'),
    ('aircraft_valuation_compatibility_projections'),
    ('listing_verification_run_items')
),
retired_relations(name) AS (
  VALUES
    ('aircraft_model_spec_versions'),
    ('aircraft_model_variant_price_points'),
    ('aircraft_model_variant_default_avionics'),
    ('aircraft_model_variant_default_avionics_candidates'),
    ('depreciation_profiles'),
    ('depreciation_profile_fit_metadata'),
    ('component_depreciation_profiles')
),
owned_relations(name) AS (
  SELECT name FROM protected_relations
  UNION ALL SELECT name FROM retired_relations
)
SELECT
  schema_row.type || ':' || schema_row.name AS object_key,
  COALESCE(lower(replace(replace(replace(replace(
    schema_row.sql, char(9), ''
  ), char(10), ''), char(13), ''), ' ', '')), '') AS definition
FROM sqlite_schema schema_row
WHERE (
  schema_row.type = 'table'
  AND schema_row.name IN (SELECT name FROM owned_relations)
) OR (
  schema_row.name IN (SELECT name FROM retired_relations)
  OR schema_row.tbl_name IN (SELECT name FROM retired_relations)
) OR (
  schema_row.type = 'view'
  AND schema_row.name = 'aircraft_reference_serial_key_errors'
) OR (
  schema_row.type = 'trigger'
  AND schema_row.tbl_name IN (SELECT name FROM owned_relations)
  AND schema_row.name NOT IN (
    'avionics_models_approved_concrete_model_insert',
    'avionics_models_approved_concrete_model_update'
  )
)
UNION ALL
SELECT
  'index:' || relation.name || ':' || index_row.name,
  index_row.[unique] || ':' || index_row.origin || ':' ||
    index_row.partial || ':' || COALESCE(lower(replace(replace(replace(replace(
      (SELECT sql FROM sqlite_schema WHERE type = 'index'
       AND name = index_row.name), char(9), ''
    ), char(10), ''), char(13), ''), ' ', '')), '') || ':' || COALESCE((
      SELECT group_concat(index_column.signature, ',')
      FROM (
        SELECT
          xinfo.seqno || ':' || xinfo.cid || ':' ||
          COALESCE(xinfo.name, '') || ':' || xinfo.desc || ':' ||
          xinfo.coll || ':' || xinfo.key AS signature
        FROM pragma_index_xinfo(index_row.name) xinfo
        ORDER BY xinfo.seqno
      ) index_column
    ), '')
FROM owned_relations relation
JOIN pragma_index_list(relation.name) index_row;

CREATE TEMP TABLE reference_catalog_schema_definition_preflight (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO reference_catalog_schema_definition_preflight (valid)
SELECT CASE WHEN EXISTS (
  SELECT 1 FROM schema_migration_contracts
  WHERE migration_name = '20260819_reference_catalog_cutover'
) AND (
  (SELECT count(*) FROM reference_catalog_schema_owned_objects) <>
    213
  OR EXISTS (
    SELECT object_key, definition
    FROM reference_catalog_schema_owned_objects
    EXCEPT
    SELECT object_key, definition
    FROM reference_catalog_schema_expected_objects
  )
  OR EXISTS (
    SELECT object_key, definition
    FROM reference_catalog_schema_expected_objects
    EXCEPT
    SELECT object_key, definition
    FROM reference_catalog_schema_owned_objects
  )
) THEN 0 ELSE 1 END;
DROP TABLE reference_catalog_schema_definition_preflight;
DROP TABLE reference_catalog_schema_owned_objects;

CREATE TABLE IF NOT EXISTS engine_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS engine_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  engine_manufacturer_id INTEGER NOT NULL REFERENCES engine_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  tbo_hours REAL,
  overhaul_cost_usd REAL,
  value_reference_year INTEGER,
  source_url TEXT,
  source_title TEXT,
  source_confidence TEXT
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (engine_manufacturer_id, normalized_name),
  CHECK (
    is_valuation_eligible = 0
    OR (
      evidence_kind = 'authoritative_reference'
      AND source_confidence = 'high'
      AND source_url IS NOT NULL
      AND tbo_hours > 0
      AND overhaul_cost_usd >= 0
      AND value_reference_year BETWEEN 1900 AND 2200
    )
  )
);

CREATE INDEX IF NOT EXISTS idx_engine_models_manufacturer
  ON engine_models (engine_manufacturer_id);

CREATE TABLE IF NOT EXISTS propeller_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS propeller_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  propeller_manufacturer_id INTEGER NOT NULL REFERENCES propeller_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  tbo_hours REAL,
  overhaul_cost_usd REAL,
  value_reference_year INTEGER,
  source_url TEXT,
  source_title TEXT,
  source_confidence TEXT
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  is_valuation_eligible INTEGER NOT NULL DEFAULT 0
    CHECK (is_valuation_eligible IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (propeller_manufacturer_id, normalized_name),
  CHECK (
    is_valuation_eligible = 0
    OR (
      evidence_kind = 'authoritative_reference'
      AND source_confidence = 'high'
      AND source_url IS NOT NULL
      AND tbo_hours > 0
      AND overhaul_cost_usd >= 0
      AND value_reference_year BETWEEN 1900 AND 2200
    )
  )
);

CREATE INDEX IF NOT EXISTS idx_propeller_models_manufacturer
  ON propeller_models (propeller_manufacturer_id);

CREATE TABLE IF NOT EXISTS aircraft_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS aircraft_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_manufacturer_id INTEGER NOT NULL REFERENCES aircraft_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_manufacturer_id, normalized_name)
);

CREATE TABLE IF NOT EXISTS aircraft_model_variants (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_id INTEGER NOT NULL REFERENCES aircraft_models(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_id, normalized_name)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_model_variants_model
  ON aircraft_model_variants (aircraft_model_id);

CREATE TABLE IF NOT EXISTS avionics_manufacturers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Stable equivalence key for manufacturer aliases. Display-name rows may
-- share a canonical key (for example, "Bendix King" and "BendixKing").
CREATE TABLE IF NOT EXISTS avionics_manufacturer_canonical_keys (
  avionics_manufacturer_id INTEGER PRIMARY KEY
    REFERENCES avionics_manufacturers(id) ON DELETE CASCADE,
  canonical_manufacturer_key TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (avionics_manufacturer_id, canonical_manufacturer_key),
  CHECK (length(canonical_manufacturer_key) > 0),
  CHECK (canonical_manufacturer_key = lower(canonical_manufacturer_key)),
  CHECK (canonical_manufacturer_key NOT GLOB '*[^a-z0-9]*')
);

CREATE INDEX IF NOT EXISTS idx_avionics_manufacturer_canonical_keys_lookup
  ON avionics_manufacturer_canonical_keys (canonical_manufacturer_key);

-- Approval may trust a stored manufacturer normalization only when it is the
-- deterministic compact-ASCII projection of the raw display name. The suffix
-- list and exact aliases mirror normalize_name/normalize_avionics_manufacturer_name.
CREATE VIEW IF NOT EXISTS avionics_manufacturer_normalization_contract AS
WITH separated AS (
  SELECT
    manufacturer.id AS avionics_manufacturer_id,
    manufacturer.name,
    manufacturer.normalized_name,
    ' ' || lower(replace(replace(replace(replace(
      trim(manufacturer.name), '-', ' '), '/', ' '), '.', ' '), '_', '')) || ' '
      AS raw_name_tokens
  FROM avionics_manufacturers manufacturer
),
suffix_stripped AS (
  SELECT
    separated.*,
    replace(replace(replace(replace(replace(replace(replace(replace(replace(
      separated.raw_name_tokens,
      ' co ', ' '), ' company ', ' '), ' corp ', ' '),
      ' corporation ', ' '), ' inc ', ' '), ' incorporated ', ' '),
      ' llc ', ' '), ' ltd ', ' '), ' limited ', ' ') AS raw_core_tokens
  FROM separated
)
SELECT
  avionics_manufacturer_id,
  CASE
    WHEN replace(raw_core_tokens, ' ', '')
      IN ('cessnaaircraft', 'textronaviation') THEN 'cessna'
    WHEN replace(raw_core_tokens, ' ', '')
      IN ('cirrusaircraft', 'cirrusdesign') THEN 'cirrus'
    WHEN replace(raw_core_tokens, ' ', '')
      IN ('theairplanefactory', 'slingaircraft', 'slingairplane') THEN 'sling'
    ELSE replace(raw_core_tokens, ' ', '')
  END AS deterministic_name_key,
  lower(replace(replace(replace(replace(replace(
    trim(normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    AS stored_name_key,
  CASE
    WHEN length(trim(name)) > 0
      AND name NOT GLOB '*[^A-Za-z0-9 ./_-]*'
      AND length(trim(normalized_name)) > 0
      AND normalized_name NOT GLOB '*[^A-Za-z0-9 ./_-]*'
    THEN 1 ELSE 0
  END AS uses_supported_ascii
FROM suffix_stripped;

-- SQLite exposes no trigger-depth predicate. This transaction-local-in-effect
-- guard distinguishes an FK cascade initiated by deleting the manufacturer
-- from a direct attempt to delete and replace its canonical grouping key.
CREATE TABLE IF NOT EXISTS avionics_manufacturer_canonical_key_delete_context (
  avionics_manufacturer_id INTEGER PRIMARY KEY
);

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_canonical_key_delete_begin
BEFORE DELETE ON avionics_manufacturers
BEGIN
  INSERT INTO avionics_manufacturer_canonical_key_delete_context (
    avionics_manufacturer_id
  ) VALUES (OLD.id)
  ON CONFLICT (avionics_manufacturer_id) DO NOTHING;
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_canonical_key_delete_end
AFTER DELETE ON avionics_manufacturers
BEGIN
  DELETE FROM avionics_manufacturer_canonical_key_delete_context
  WHERE avionics_manufacturer_id = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_canonical_key_insert
AFTER INSERT ON avionics_manufacturers
BEGIN
  INSERT INTO avionics_manufacturer_canonical_keys (
    avionics_manufacturer_id, canonical_manufacturer_key
  )
  VALUES (
    NEW.id,
    lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  );
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_canonical_key_immutable
BEFORE UPDATE OF canonical_manufacturer_key
ON avionics_manufacturer_canonical_keys
BEGIN
  SELECT RAISE(ABORT, 'avionics manufacturer canonical key is immutable');
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_canonical_key_delete
BEFORE DELETE ON avionics_manufacturer_canonical_keys
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_manufacturer_canonical_key_delete_context delete_context
  WHERE delete_context.avionics_manufacturer_id = OLD.avionics_manufacturer_id
)
BEGIN
  SELECT RAISE(ABORT, 'avionics manufacturer canonical key cannot be deleted directly');
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_normalized_name_preserve_key
BEFORE UPDATE OF normalized_name ON avionics_manufacturers
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_manufacturer_canonical_keys manufacturer_key
  WHERE manufacturer_key.avionics_manufacturer_id = OLD.id
    AND manufacturer_key.canonical_manufacturer_key = lower(replace(replace(
      replace(replace(replace(trim(NEW.normalized_name), ' ', ''), '-', ''),
      '/', ''), '.', ''), '_', ''))
)
BEGIN
  SELECT RAISE(ABORT, 'manufacturer normalization cannot change its canonical key');
END;

-- Evidence-backed manufacturer identity is distinct from display spelling.
-- The canonical-key table above is only a deterministic retrieval key; it may
-- create exact-safe memberships but never authorizes a semantic alias.
CREATE TABLE IF NOT EXISTS avionics_manufacturer_identities (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  canonical_name TEXT NOT NULL,
  normalized_identity_key TEXT NOT NULL UNIQUE,
  identity_evidence_kind TEXT NOT NULL
    CHECK (identity_evidence_kind = 'authoritative_reference'),
  identity_source_url TEXT NOT NULL,
  identity_source_title TEXT NOT NULL,
  identity_evidence_text TEXT NOT NULL,
  identity_confidence TEXT NOT NULL CHECK (identity_confidence = 'very_high'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(canonical_name)) > 0),
  CHECK (length(normalized_identity_key) > 0),
  CHECK (normalized_identity_key = lower(normalized_identity_key)),
  CHECK (normalized_identity_key NOT GLOB '*[^a-z0-9]*'),
  CHECK (length(trim(identity_source_url)) > 0),
  CHECK (length(trim(identity_source_title)) > 0),
  CHECK (length(trim(identity_evidence_text)) > 0),
  CHECK (lower(identity_source_url) LIKE 'https://%')
);

-- An authority row grants only one exact HTTPS origin. It does not grant a
-- parent domain, sibling host, or any unrecorded subdomain. Manufacturer
-- origins attach to immutable identity rows so approved aliases can inherit
-- them through the effective-identity graph without copying mutable state.
CREATE TABLE IF NOT EXISTS avionics_authoritative_source_origins (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  authority_kind TEXT NOT NULL CHECK (authority_kind IN (
    'manufacturer_primary', 'regulator_primary'
  )),
  avionics_manufacturer_identity_id INTEGER
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  regulator_key TEXT,
  https_origin TEXT NOT NULL,
  evidence_source_url TEXT NOT NULL,
  evidence_source_title TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  approval_basis TEXT NOT NULL CHECK (approval_basis IN (
    'curated_bootstrap', 'human_review'
  )),
  approved_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
  approval_reason TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (
      authority_kind = 'manufacturer_primary'
      AND avionics_manufacturer_identity_id IS NOT NULL
      AND regulator_key IS NULL
    )
    OR (
      authority_kind = 'regulator_primary'
      AND avionics_manufacturer_identity_id IS NULL
      AND regulator_key IS NOT NULL
      AND length(regulator_key) > 0
      AND regulator_key = lower(regulator_key)
      AND regulator_key NOT GLOB '*[^a-z0-9_]*'
    )
  ),
  CHECK (https_origin = lower(trim(https_origin))),
  CHECK (substr(https_origin, 1, 8) = 'https://'),
  CHECK (length(substr(https_origin, 9)) >= 3),
  CHECK (instr(substr(https_origin, 9), '.') > 1),
  CHECK (substr(https_origin, 9) NOT GLOB '*[^a-z0-9.-]*'),
  CHECK (instr(substr(https_origin, 9), '..') = 0),
  CHECK (instr(substr(https_origin, 9), '.-') = 0),
  CHECK (instr(substr(https_origin, 9), '-.') = 0),
  CHECK (substr(substr(https_origin, 9), 1, 1) NOT IN ('.', '-')),
  CHECK (substr(https_origin, -1, 1) NOT IN ('.', '-')),
  CHECK (
    evidence_source_url = https_origin
    OR (
      substr(evidence_source_url, 1, length(https_origin)) = https_origin
      AND substr(evidence_source_url, length(https_origin) + 1, 1) = '/'
    )
  ),
  CHECK (length(trim(evidence_source_title)) >= 4),
  CHECK (length(trim(evidence_text)) >= 20),
  CHECK (
    (approval_basis = 'curated_bootstrap' AND approved_by_user_id IS NULL)
    OR (approval_basis = 'human_review' AND approved_by_user_id IS NOT NULL)
  ),
  CHECK (length(trim(approval_reason)) >= 10)
);

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_avionics_authoritative_origin_manufacturer
ON avionics_authoritative_source_origins (
  avionics_manufacturer_identity_id, https_origin
)
WHERE authority_kind = 'manufacturer_primary';

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_avionics_authoritative_origin_regulator
ON avionics_authoritative_source_origins (regulator_key, https_origin)
WHERE authority_kind = 'regulator_primary';

CREATE INDEX IF NOT EXISTS idx_avionics_authoritative_origin_lookup
  ON avionics_authoritative_source_origins (
    authority_kind, https_origin, avionics_manufacturer_identity_id
  );

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origins_immutable_update
BEFORE UPDATE ON avionics_authoritative_source_origins
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source origins are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origins_immutable_delete
BEFORE DELETE ON avionics_authoritative_source_origins
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source-origin approvals are permanent audit records');
END;

CREATE TABLE IF NOT EXISTS avionics_authoritative_source_origin_revocations (
  avionics_authoritative_source_origin_id INTEGER PRIMARY KEY
    REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT,
  revoked_by_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  reason TEXT NOT NULL,
  revoked_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(reason)) >= 10)
);

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origin_revocations_immutable_update
BEFORE UPDATE ON avionics_authoritative_source_origin_revocations
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source-origin revocations are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_authoritative_source_origin_revocations_immutable_delete
BEFORE DELETE ON avionics_authoritative_source_origin_revocations
BEGIN
  SELECT RAISE(ABORT, 'avionics authoritative source-origin revocations are permanent audit records');
END;

CREATE VIEW IF NOT EXISTS avionics_active_authoritative_source_origins AS
SELECT source_origin.*
FROM avionics_authoritative_source_origins source_origin
WHERE NOT EXISTS (
  SELECT 1
  FROM avionics_authoritative_source_origin_revocations revocation
  WHERE revocation.avionics_authoritative_source_origin_id = source_origin.id
);

-- A fresh schema has no curated identities yet. Provision the two reviewed,
-- fixed Garmin origins only when the specific Garmin identity is later
-- inserted; never derive an authority origin from arbitrary evidence URLs.
CREATE TRIGGER IF NOT EXISTS
  avionics_garmin_authoritative_source_origins_bootstrap
AFTER INSERT ON avionics_manufacturer_identities
WHEN NEW.normalized_identity_key = 'garmin'
  AND lower(trim(NEW.canonical_name)) = 'garmin'
  AND NEW.identity_evidence_kind = 'authoritative_reference'
  AND NEW.identity_confidence = 'very_high'
  AND substr(NEW.identity_source_url, 1, 23) =
    'https://www.garmin.com/'
BEGIN
  INSERT INTO avionics_authoritative_source_origins (
    authority_kind,
    avionics_manufacturer_identity_id,
    regulator_key,
    https_origin,
    evidence_source_url,
    evidence_source_title,
    evidence_text,
    approval_basis,
    approved_by_user_id,
    approval_reason
  ) VALUES (
    'manufacturer_primary',
    NEW.id,
    NULL,
    'https://www.garmin.com',
    'https://www.garmin.com/en-US/p/588901/',
    'Garmin G1000 NXi | Integrated Flight Deck',
    'The Garmin G1000 NXi is an advanced integrated flight deck family designed and manufactured by Garmin',
    'curated_bootstrap',
    NULL,
    'Reviewed first-party Garmin origin bootstrap installed by the 20260801 migration'
  )
  ON CONFLICT DO NOTHING;

  INSERT INTO avionics_authoritative_source_origins (
    authority_kind,
    avionics_manufacturer_identity_id,
    regulator_key,
    https_origin,
    evidence_source_url,
    evidence_source_title,
    evidence_text,
    approval_basis,
    approved_by_user_id,
    approval_reason
  ) VALUES (
    'manufacturer_primary',
    NEW.id,
    NULL,
    'https://static.garmin.com',
    'https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf',
    'Garmin GIA 63/GIA 63W Installation Manual',
    'GIA 63W Unit Only, (011-01105-00) 010-00386-00',
    'curated_bootstrap',
    NULL,
    'Reviewed first-party Garmin origin bootstrap installed by the 20260801 migration'
  )
  ON CONFLICT DO NOTHING;
END;

CREATE TABLE IF NOT EXISTS avionics_manufacturer_identity_memberships (
  avionics_manufacturer_id INTEGER PRIMARY KEY
    REFERENCES avionics_manufacturers(id) ON DELETE RESTRICT,
  avionics_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  membership_basis TEXT NOT NULL CHECK (membership_basis IN (
    'deterministic_exact', 'authoritative_primary', 'authoritative_alias'
  )),
  normalized_name_key TEXT NOT NULL,
  evidence_source_url TEXT NOT NULL,
  evidence_source_title TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  evidence_confidence TEXT NOT NULL CHECK (evidence_confidence = 'very_high'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(normalized_name_key) > 0),
  CHECK (normalized_name_key = lower(normalized_name_key)),
  CHECK (normalized_name_key NOT GLOB '*[^a-z0-9]*'),
  CHECK (length(trim(evidence_source_url)) > 0),
  CHECK (length(trim(evidence_source_title)) > 0),
  CHECK (length(trim(evidence_text)) > 0)
);

CREATE INDEX IF NOT EXISTS idx_avionics_manufacturer_identity_memberships_group
  ON avionics_manufacturer_identity_memberships (
    avionics_manufacturer_identity_id, avionics_manufacturer_id
  );

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_identity_immutable_update
BEFORE UPDATE ON avionics_manufacturer_identities
BEGIN SELECT RAISE(ABORT, 'approved avionics manufacturer identities are immutable'); END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_identity_immutable_delete
BEFORE DELETE ON avionics_manufacturer_identities
BEGIN SELECT RAISE(ABORT, 'approved avionics manufacturer identities are immutable'); END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_membership_validate_insert
BEFORE INSERT ON avionics_manufacturer_identity_memberships
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_manufacturer_canonical_keys manufacturer_key
  JOIN avionics_manufacturer_normalization_contract normalization
    ON normalization.avionics_manufacturer_id
      = manufacturer_key.avionics_manufacturer_id
  JOIN avionics_manufacturer_identities identity
    ON identity.id = NEW.avionics_manufacturer_identity_id
  WHERE manufacturer_key.avionics_manufacturer_id = NEW.avionics_manufacturer_id
    AND manufacturer_key.canonical_manufacturer_key = NEW.normalized_name_key
    AND normalization.uses_supported_ascii = 1
    AND normalization.deterministic_name_key
      = normalization.stored_name_key
    AND normalization.stored_name_key
      = manufacturer_key.canonical_manufacturer_key
    AND (
      NEW.membership_basis = 'authoritative_alias'
      OR (
        NEW.normalized_name_key = identity.normalized_identity_key
        AND (
          NEW.membership_basis = 'authoritative_primary'
          OR (
            NEW.membership_basis = 'deterministic_exact'
            AND NEW.evidence_source_url =
              'urn:aircost:deterministic:avionics-manufacturer-normalization:v1'
          )
        )
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'manufacturer membership lacks exact normalization or authoritative alias evidence');
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_membership_immutable_update
BEFORE UPDATE ON avionics_manufacturer_identity_memberships
BEGIN SELECT RAISE(ABORT, 'avionics manufacturer identity memberships are immutable'); END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_membership_immutable_delete
BEFORE DELETE ON avionics_manufacturer_identity_memberships
BEGIN SELECT RAISE(ABORT, 'avionics manufacturer identity memberships are immutable'); END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_identity_name_immutable
BEFORE UPDATE OF name, normalized_name ON avionics_manufacturers
WHEN EXISTS (
  SELECT 1
  FROM avionics_manufacturer_identity_memberships membership
  WHERE membership.avionics_manufacturer_id = OLD.id
)
AND (
  NEW.name IS NOT OLD.name
  OR NEW.normalized_name IS NOT OLD.normalized_name
)
BEGIN
  SELECT RAISE(ABORT, 'evidence-backed avionics manufacturer name is immutable');
END;

CREATE TABLE IF NOT EXISTS avionics_types (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS avionics_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  avionics_manufacturer_id INTEGER NOT NULL REFERENCES avionics_manufacturers(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  catalog_status TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (catalog_status IN ('unreviewed', 'approved', 'rejected')),
  manufacturer_identifier_kind TEXT
    CHECK (
      manufacturer_identifier_kind IS NULL
      OR manufacturer_identifier_kind IN (
        'manufacturer_part_number', 'manufacturer_model_number', 'sku'
      )
    ),
  manufacturer_identifier TEXT,
  normalized_manufacturer_identifier TEXT,
  identity_source_url TEXT,
  identity_source_title TEXT,
  identity_evidence_text TEXT,
  identity_evidence_kind TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (identity_evidence_kind IN ('authoritative_reference', 'listing_only', 'unreviewed')),
  identity_confidence TEXT
    CHECK (identity_confidence IS NULL OR identity_confidence IN ('very_high', 'high', 'medium', 'low')),
  catalog_reviewed_at TEXT,
  introduced_year INTEGER,
  discontinued_year INTEGER,
  estimated_unit_value_usd REAL,
  value_basis TEXT NOT NULL DEFAULT 'unreviewed'
    CHECK (value_basis IN ('installed_contribution', 'replacement_cost', 'unreviewed')),
  replacement_cost_usd REAL,
  value_reference_year INTEGER,
  value_source TEXT,
  valuation_scope TEXT NOT NULL DEFAULT 'unit'
    CHECK (valuation_scope IN ('unit', 'integrated_suite')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (
      manufacturer_identifier_kind IS NULL
      AND manufacturer_identifier IS NULL
      AND normalized_manufacturer_identifier IS NULL
    )
    OR (
      manufacturer_identifier_kind IS NOT NULL
      AND manufacturer_identifier IS NOT NULL
      AND length(trim(manufacturer_identifier)) > 0
      AND normalized_manufacturer_identifier IS NOT NULL
      AND length(trim(normalized_manufacturer_identifier)) > 0
    )
  ),
  CHECK (
    catalog_status = 'unreviewed'
    OR (catalog_reviewed_at IS NOT NULL AND length(trim(catalog_reviewed_at)) > 0)
  ),
  CHECK (
    catalog_status <> 'approved'
    OR (
      length(trim(name)) > 0
      AND length(trim(normalized_name)) > 0
      AND lower(trim(normalized_name)) NOT IN (
        'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
        'avionics', 'avionics suite', 'integrated avionics',
        'integrated avionics suite', 'glass panel', 'flight instruments',
        'standard flight instruments', 'standard vfr avionics',
        'standard ifr avionics', 'radio', 'radios', 'nav com',
        'navigation system', 'gps', 'autopilot', 'transponder', 'ads b',
        'weather radar', 'audio panel', 'display', 'equipment'
      )
      AND instr(' ' || lower(trim(normalized_name)) || ' ', ' series ') = 0
      AND instr(' ' || lower(trim(normalized_name)) || ' ', ' family ') = 0
      AND manufacturer_identifier_kind IS NOT NULL
      AND manufacturer_identifier IS NOT NULL
      AND length(trim(manufacturer_identifier)) > 0
      AND normalized_manufacturer_identifier IS NOT NULL
      AND length(trim(normalized_manufacturer_identifier)) > 0
      AND identity_source_url IS NOT NULL
      AND length(trim(identity_source_url)) > 0
      AND identity_source_title IS NOT NULL
      AND length(trim(identity_source_title)) > 0
      AND identity_evidence_text IS NOT NULL
      AND length(trim(identity_evidence_text)) > 0
      AND identity_evidence_kind = 'authoritative_reference'
      AND identity_confidence = 'very_high'
      AND catalog_reviewed_at IS NOT NULL
      AND length(trim(catalog_reviewed_at)) > 0
      AND lower(identity_source_url) NOT LIKE '%/listing/%'
      AND lower(identity_source_url) NOT LIKE '%/listings/%'
      AND lower(identity_source_url) NOT LIKE '%/aircraft-for-sale/%'
      AND lower(identity_source_url) NOT LIKE '%/classifieds/%'
    )
  ),
  CHECK (
    value_basis <> 'installed_contribution'
    OR (
      estimated_unit_value_usd >= 0
      AND replacement_cost_usd >= estimated_unit_value_usd
      AND value_reference_year BETWEEN 1900 AND 2200
      AND value_source IS NOT NULL
      AND length(trim(value_source)) > 0
    )
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_avionics_models_manufacturer_identifier
  ON avionics_models (
    avionics_manufacturer_id,
    manufacturer_identifier_kind,
    normalized_manufacturer_identifier
  )
  WHERE normalized_manufacturer_identifier IS NOT NULL
    AND length(trim(normalized_manufacturer_identifier)) > 0;

-- Legacy unreviewed rows can still contain same-name candidates that require
-- evidence-based consolidation. Approved product identities cannot.
CREATE UNIQUE INDEX IF NOT EXISTS idx_avionics_models_approved_manufacturer_name
  ON avionics_models (avionics_manufacturer_id, normalized_name)
  WHERE catalog_status = 'approved';

-- Semantic aliases are review candidates, never implicit memberships.
CREATE TABLE IF NOT EXISTS avionics_manufacturer_alias_candidates (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  avionics_manufacturer_id INTEGER NOT NULL
    REFERENCES avionics_manufacturers(id) ON DELETE RESTRICT,
  candidate_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  candidate_basis TEXT NOT NULL CHECK (candidate_basis IN (
    'exact_product_name', 'exact_stable_identifier',
    'semantic_similarity', 'grounded_alias'
  )),
  matched_avionics_model_id INTEGER
    REFERENCES avionics_models(id) ON DELETE SET NULL,
  reason TEXT NOT NULL,
  evidence_source_url TEXT,
  evidence_source_title TEXT,
  evidence_text TEXT,
  confidence TEXT NOT NULL CHECK (confidence IN (
    'very_high', 'high', 'medium', 'low'
  )),
  review_status TEXT NOT NULL DEFAULT 'pending'
    CHECK (review_status IN ('pending', 'approved', 'rejected')),
  decision_reason TEXT,
  decision_evidence_source_url TEXT,
  decision_evidence_source_title TEXT,
  decision_evidence_text TEXT,
  reviewed_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
  reviewed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(reason)) > 0),
  CHECK (
    (evidence_source_url IS NULL
      AND evidence_source_title IS NULL
      AND evidence_text IS NULL)
    OR (evidence_source_url IS NOT NULL
      AND lower(evidence_source_url) LIKE 'https://%'
      AND evidence_source_title IS NOT NULL
      AND length(trim(evidence_source_title)) > 0
      AND evidence_text IS NOT NULL
      AND length(trim(evidence_text)) > 0)
  ),
  CHECK (
    (review_status = 'pending'
      AND decision_reason IS NULL
      AND decision_evidence_source_url IS NULL
      AND decision_evidence_source_title IS NULL
      AND decision_evidence_text IS NULL
      AND reviewed_by_user_id IS NULL
      AND reviewed_at IS NULL)
    OR (review_status = 'rejected'
      AND decision_reason IS NOT NULL
      AND length(trim(decision_reason)) > 0
      AND reviewed_by_user_id IS NOT NULL
      AND reviewed_at IS NOT NULL)
    OR (review_status = 'approved'
      AND decision_reason IS NOT NULL
      AND length(trim(decision_reason)) > 0
      AND decision_evidence_source_url IS NOT NULL
      AND lower(decision_evidence_source_url) LIKE 'https://%'
      AND decision_evidence_source_title IS NOT NULL
      AND length(trim(decision_evidence_source_title)) > 0
      AND decision_evidence_text IS NOT NULL
      AND length(trim(decision_evidence_text)) > 0
      AND reviewed_by_user_id IS NOT NULL
      AND reviewed_at IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_avionics_manufacturer_alias_candidates_pending
  ON avionics_manufacturer_alias_candidates (
    avionics_manufacturer_id, candidate_manufacturer_identity_id
  )
  WHERE review_status = 'pending';

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_alias_candidate_pending_insert
BEFORE INSERT ON avionics_manufacturer_alias_candidates
WHEN NEW.review_status <> 'pending'
BEGIN
  SELECT RAISE(ABORT, 'manufacturer alias candidates must be inserted pending');
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_alias_candidate_update
BEFORE UPDATE ON avionics_manufacturer_alias_candidates
WHEN NOT (
    NEW.id = OLD.id
    AND NEW.avionics_manufacturer_id = OLD.avionics_manufacturer_id
    AND NEW.candidate_manufacturer_identity_id
      = OLD.candidate_manufacturer_identity_id
    AND NEW.candidate_basis = OLD.candidate_basis
    AND NEW.matched_avionics_model_id IS NOT OLD.matched_avionics_model_id
    AND NEW.reason = OLD.reason
    AND NEW.evidence_source_url IS OLD.evidence_source_url
    AND NEW.evidence_source_title IS OLD.evidence_source_title
    AND NEW.evidence_text IS OLD.evidence_text
    AND NEW.confidence = OLD.confidence
    AND NEW.review_status = OLD.review_status
    AND NEW.decision_reason IS OLD.decision_reason
    AND NEW.decision_evidence_source_url IS OLD.decision_evidence_source_url
    AND NEW.decision_evidence_source_title IS OLD.decision_evidence_source_title
    AND NEW.decision_evidence_text IS OLD.decision_evidence_text
    AND NEW.reviewed_by_user_id IS OLD.reviewed_by_user_id
    AND NEW.reviewed_at IS OLD.reviewed_at
    AND NEW.created_at = OLD.created_at
    AND EXISTS (
      SELECT 1
      FROM avionics_catalog_authorized_consolidations guard
      WHERE guard.duplicate_model_id = OLD.matched_avionics_model_id
        AND guard.survivor_model_id = NEW.matched_avionics_model_id
    )
  )
  AND (
    OLD.review_status <> 'pending'
    OR NEW.id <> OLD.id
    OR NEW.avionics_manufacturer_id <> OLD.avionics_manufacturer_id
    OR NEW.candidate_manufacturer_identity_id
      <> OLD.candidate_manufacturer_identity_id
    OR NEW.candidate_basis <> OLD.candidate_basis
    OR NEW.matched_avionics_model_id IS NOT OLD.matched_avionics_model_id
    OR NEW.reason <> OLD.reason
    OR NEW.evidence_source_url IS NOT OLD.evidence_source_url
    OR NEW.evidence_source_title IS NOT OLD.evidence_source_title
    OR NEW.evidence_text IS NOT OLD.evidence_text
    OR NEW.confidence <> OLD.confidence
    OR NEW.review_status NOT IN ('approved', 'rejected')
  )
BEGIN
  SELECT RAISE(ABORT, 'manufacturer alias candidates are immutable after staging except for one review decision');
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_alias_candidate_delete
BEFORE DELETE ON avionics_manufacturer_alias_candidates
BEGIN SELECT RAISE(ABORT, 'manufacturer alias candidate history is immutable'); END;

-- Original identities and memberships remain immutable. A semantic merge is
-- an append-only redirect from one current root to another current root.
CREATE TABLE IF NOT EXISTS avionics_manufacturer_identity_merges (
  merged_identity_id INTEGER PRIMARY KEY
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  survivor_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  alias_candidate_id INTEGER NOT NULL UNIQUE
    REFERENCES avionics_manufacturer_alias_candidates(id) ON DELETE RESTRICT,
  evidence_source_url TEXT NOT NULL,
  evidence_source_title TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  decided_by_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (merged_identity_id <> survivor_identity_id),
  CHECK (lower(evidence_source_url) LIKE 'https://%'),
  CHECK (length(trim(evidence_source_title)) > 0),
  CHECK (length(trim(evidence_text)) > 0)
);

CREATE VIEW IF NOT EXISTS avionics_manufacturer_effective_identities AS
WITH RECURSIVE resolved(identity_id, effective_identity_id, depth, path) AS (
  SELECT identity.id, identity.id, 0, ',' || identity.id || ','
  FROM avionics_manufacturer_identities identity
  UNION ALL
  SELECT resolved.identity_id, merge.survivor_identity_id,
         resolved.depth + 1,
         resolved.path || merge.survivor_identity_id || ','
  FROM resolved
  JOIN avionics_manufacturer_identity_merges merge
    ON merge.merged_identity_id = resolved.effective_identity_id
  WHERE resolved.depth < 32
    AND instr(resolved.path, ',' || merge.survivor_identity_id || ',') = 0
)
SELECT resolved.identity_id,
       resolved.effective_identity_id AS avionics_manufacturer_identity_id
FROM resolved
WHERE NOT EXISTS (
  SELECT 1 FROM avionics_manufacturer_identity_merges merge
  WHERE merge.merged_identity_id = resolved.effective_identity_id
);

CREATE VIEW IF NOT EXISTS avionics_manufacturer_effective_memberships AS
SELECT membership.avionics_manufacturer_id,
       membership.avionics_manufacturer_identity_id AS original_identity_id,
       effective.avionics_manufacturer_identity_id
FROM avionics_manufacturer_identity_memberships membership
JOIN avionics_manufacturer_effective_identities effective
  ON effective.identity_id = membership.avionics_manufacturer_identity_id;

-- Curated exact-origin bootstrap. These independent rows do not authorize the
-- garmin.com parent or any other subdomain.
INSERT INTO avionics_authoritative_source_origins (
  authority_kind,
  avionics_manufacturer_identity_id,
  regulator_key,
  https_origin,
  evidence_source_url,
  evidence_source_title,
  evidence_text,
  approval_basis,
  approved_by_user_id,
  approval_reason
)
SELECT
  'manufacturer_primary',
  identity.id,
  NULL,
  seed.https_origin,
  seed.evidence_source_url,
  seed.evidence_source_title,
  seed.evidence_text,
  'curated_bootstrap',
  NULL,
  'Reviewed first-party Garmin origin bootstrap installed by the 20260801 migration'
FROM avionics_manufacturer_identities identity
JOIN (
  SELECT
    'https://www.garmin.com' AS https_origin,
    'https://www.garmin.com/en-US/p/588901/' AS evidence_source_url,
    'Garmin G1000 NXi | Integrated Flight Deck' AS evidence_source_title,
    'The Garmin G1000 NXi is an advanced integrated flight deck family designed and manufactured by Garmin'
      AS evidence_text
  UNION ALL
  SELECT
    'https://static.garmin.com',
    'https://static.garmin.com/pumac/GIA63_GIA63W_InstallationManual.pdf',
    'Garmin GIA 63/GIA 63W Installation Manual',
    'GIA 63W Unit Only, (011-01105-00) 010-00386-00'
) seed
WHERE identity.normalized_identity_key = 'garmin'
  AND lower(trim(identity.canonical_name)) = 'garmin'
  AND substr(identity.identity_source_url, 1, 23) =
    'https://www.garmin.com/'
ON CONFLICT DO NOTHING;

-- Legacy catalog rows can suggest an alias but cannot establish one. The view
-- is deliberately directional so, once either side gains authoritative
-- identity evidence, the other raw maker can be staged for review.
CREATE VIEW IF NOT EXISTS avionics_legacy_manufacturer_alias_signals AS
WITH products AS (
  SELECT model.id AS avionics_model_id,
         model.avionics_manufacturer_id,
         manufacturer.name AS manufacturer,
         model.name AS model,
         lower(replace(replace(replace(replace(replace(
           trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
           AS canonical_product_key,
         model.manufacturer_identifier_kind,
         CASE
           WHEN model.normalized_manufacturer_identifier IS NULL THEN NULL
           ELSE lower(replace(replace(replace(replace(replace(
             trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''),
             '/', ''), '.', ''), '_', ''))
         END AS canonical_identifier_key
  FROM avionics_models model
  JOIN avionics_manufacturers manufacturer
    ON manufacturer.id = model.avionics_manufacturer_id
  WHERE model.catalog_status <> 'rejected'
)
SELECT
  CASE
    WHEN left_product.manufacturer_identifier_kind IS NOT NULL
      AND left_product.manufacturer_identifier_kind
        = right_product.manufacturer_identifier_kind
      AND length(left_product.canonical_identifier_key) > 0
      AND left_product.canonical_identifier_key
        = right_product.canonical_identifier_key
    THEN 'exact_stable_identifier'
    ELSE 'exact_product_name'
  END AS candidate_basis,
  left_product.avionics_manufacturer_id AS left_avionics_manufacturer_id,
  left_product.manufacturer AS left_manufacturer,
  left_product.avionics_model_id AS left_avionics_model_id,
  left_product.model AS left_model,
  right_product.avionics_manufacturer_id AS right_avionics_manufacturer_id,
  right_product.manufacturer AS right_manufacturer,
  right_product.avionics_model_id AS right_avionics_model_id,
  right_product.model AS right_model
FROM products left_product
JOIN products right_product
  ON right_product.avionics_manufacturer_id
    <> left_product.avionics_manufacturer_id
 AND (
   (
     left_product.manufacturer_identifier_kind IS NOT NULL
     AND left_product.manufacturer_identifier_kind
       = right_product.manufacturer_identifier_kind
     AND length(left_product.canonical_identifier_key) > 0
     AND left_product.canonical_identifier_key
       = right_product.canonical_identifier_key
   )
   OR (
     length(left_product.canonical_product_key) > 0
     AND left_product.canonical_product_key
       = right_product.canonical_product_key
   )
 );

-- Only approved products occupy this identity registry. Its uniqueness is
-- based on the evidence-backed effective manufacturer identity, never a raw
-- display row or caller-normalized label.
CREATE TABLE IF NOT EXISTS avionics_approved_product_identities (
  avionics_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  canonical_product_key TEXT NOT NULL,
  manufacturer_identifier_kind TEXT NOT NULL
    CHECK (manufacturer_identifier_kind IN (
      'manufacturer_part_number', 'manufacturer_model_number', 'sku'
    )),
  canonical_identifier_key TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (avionics_manufacturer_identity_id, canonical_product_key),
  UNIQUE (
    avionics_manufacturer_identity_id, manufacturer_identifier_kind,
    canonical_identifier_key
  ),
  CHECK (length(canonical_product_key) > 0),
  CHECK (canonical_product_key = lower(canonical_product_key)),
  CHECK (canonical_product_key NOT GLOB '*[^a-z0-9]*'),
  CHECK (length(canonical_identifier_key) > 0),
  CHECK (canonical_identifier_key = lower(canonical_identifier_key)),
  CHECK (canonical_identifier_key NOT GLOB '*[^a-z0-9]*')
);

CREATE VIEW IF NOT EXISTS avionics_approved_product_graph_identities AS
SELECT avionics_model_id, avionics_manufacturer_identity_id,
       canonical_product_key, manufacturer_identifier_kind,
       canonical_identifier_key
FROM avionics_approved_product_identities;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_alias_membership_requires_decision
BEFORE INSERT ON avionics_manufacturer_identity_memberships
WHEN NEW.membership_basis = 'authoritative_alias'
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_manufacturer_alias_candidates candidate
    JOIN avionics_manufacturer_effective_identities effective
      ON effective.identity_id = candidate.candidate_manufacturer_identity_id
    WHERE candidate.avionics_manufacturer_id = NEW.avionics_manufacturer_id
      AND candidate.review_status = 'approved'
      AND effective.avionics_manufacturer_identity_id
        = NEW.avionics_manufacturer_identity_id
      AND candidate.decision_evidence_source_url = NEW.evidence_source_url
      AND candidate.decision_evidence_source_title = NEW.evidence_source_title
      AND candidate.decision_evidence_text = NEW.evidence_text
  )
BEGIN
  SELECT RAISE(ABORT, 'semantic manufacturer membership requires an approved authoritative alias decision');
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_identity_merge_validate
BEFORE INSERT ON avionics_manufacturer_identity_merges
WHEN EXISTS (
    SELECT 1 FROM avionics_manufacturer_identity_merges existing
    WHERE existing.merged_identity_id = NEW.merged_identity_id
       OR existing.merged_identity_id = NEW.survivor_identity_id
  )
  OR EXISTS (
    WITH RECURSIVE incoming(identity_id, depth) AS (
      SELECT NEW.merged_identity_id, 0
      UNION ALL
      SELECT existing.merged_identity_id, incoming.depth + 1
      FROM avionics_manufacturer_identity_merges existing
      JOIN incoming
        ON existing.survivor_identity_id = incoming.identity_id
      WHERE incoming.depth < 32
    )
    SELECT 1 FROM incoming WHERE depth = 32
  )
  OR NOT EXISTS (
    SELECT 1
    FROM avionics_manufacturer_alias_candidates candidate
    JOIN avionics_manufacturer_effective_memberships membership
      ON membership.avionics_manufacturer_id = candidate.avionics_manufacturer_id
    JOIN avionics_manufacturer_effective_identities candidate_target
      ON candidate_target.identity_id = candidate.candidate_manufacturer_identity_id
    WHERE candidate.id = NEW.alias_candidate_id
      AND candidate.review_status = 'approved'
      AND membership.avionics_manufacturer_identity_id = NEW.merged_identity_id
      AND candidate_target.avionics_manufacturer_identity_id
        = NEW.survivor_identity_id
      AND candidate.decision_evidence_source_url = NEW.evidence_source_url
      AND candidate.decision_evidence_source_title = NEW.evidence_source_title
      AND candidate.decision_evidence_text = NEW.evidence_text
  )
  OR EXISTS (
    SELECT 1
    FROM avionics_approved_product_identities merged_product
    JOIN avionics_approved_product_identities survivor_product
      ON survivor_product.avionics_manufacturer_identity_id
        = NEW.survivor_identity_id
     AND (
       survivor_product.canonical_product_key
         = merged_product.canonical_product_key
       OR (
         survivor_product.manufacturer_identifier_kind
           = merged_product.manufacturer_identifier_kind
         AND survivor_product.canonical_identifier_key
           = merged_product.canonical_identifier_key
       )
     )
    WHERE merged_product.avionics_manufacturer_identity_id
      = NEW.merged_identity_id
  )
BEGIN
  SELECT RAISE(ABORT, 'manufacturer identity merge requires two roots, an approved alias decision, and no product collision');
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_identity_merge_apply
AFTER INSERT ON avionics_manufacturer_identity_merges
BEGIN
  UPDATE avionics_approved_product_identities
  SET avionics_manufacturer_identity_id = NEW.survivor_identity_id,
      updated_at = CURRENT_TIMESTAMP
  WHERE avionics_manufacturer_identity_id = NEW.merged_identity_id;
END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_identity_merge_immutable_update
BEFORE UPDATE ON avionics_manufacturer_identity_merges
BEGIN SELECT RAISE(ABORT, 'manufacturer identity merge history is immutable'); END;

CREATE TRIGGER IF NOT EXISTS avionics_manufacturer_identity_merge_immutable_delete
BEFORE DELETE ON avionics_manufacturer_identity_merges
BEGIN SELECT RAISE(ABORT, 'manufacturer identity merge history is immutable'); END;

-- A minimal, durable human adjudication record for exact same-product
-- consolidation. This stores only the reviewer, authoritative source excerpt,
-- optional review provenance, and exact catalog-row identity snapshots. It
-- intentionally stores no Gemini prompt, response, or retrieval dossier.
CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_authorizations (
  authorization_sha256 TEXT PRIMARY KEY,
  reviewer_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  survivor_model_id_snapshot INTEGER NOT NULL,
  effective_manufacturer_identity_id_snapshot INTEGER NOT NULL,
  canonical_model_key_snapshot TEXT NOT NULL,
  expected_member_count INTEGER NOT NULL,
  authoritative_source_url TEXT NOT NULL,
  authoritative_source_title TEXT NOT NULL,
  exact_evidence_text TEXT NOT NULL,
  provenance_listing_id_snapshot INTEGER,
  provenance_pending_review_id_snapshot INTEGER,
  provenance_review_payload_sha256 TEXT,
  provenance_review_aspect_id TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(authorization_sha256) = 64),
  CHECK (authorization_sha256 = lower(authorization_sha256)),
  CHECK (authorization_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (survivor_model_id_snapshot > 0),
  CHECK (effective_manufacturer_identity_id_snapshot > 0),
  CHECK (length(trim(canonical_model_key_snapshot)) > 0),
  CHECK (canonical_model_key_snapshot = lower(canonical_model_key_snapshot)),
  CHECK (canonical_model_key_snapshot NOT GLOB '*[^a-z0-9 ]*'),
  CHECK (expected_member_count >= 2),
  CHECK (authoritative_source_url LIKE 'https://%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/listing/%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/listings/%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/aircraft-for-sale/%'),
  CHECK (lower(authoritative_source_url) NOT LIKE '%/classifieds/%'),
  CHECK (length(trim(authoritative_source_title)) > 0),
  CHECK (length(trim(exact_evidence_text)) > 0),
  CHECK (
    (
      provenance_listing_id_snapshot IS NULL
      AND provenance_pending_review_id_snapshot IS NULL
      AND provenance_review_payload_sha256 IS NULL
      AND provenance_review_aspect_id IS NULL
    )
    OR (
      provenance_listing_id_snapshot > 0
      AND provenance_pending_review_id_snapshot > 0
      AND length(provenance_review_payload_sha256) = 64
      AND provenance_review_payload_sha256 = lower(provenance_review_payload_sha256)
      AND provenance_review_payload_sha256 NOT GLOB '*[^0-9a-f]*'
      AND length(trim(provenance_review_aspect_id)) > 0
    )
  )
);

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_members (
  authorization_sha256 TEXT NOT NULL
    REFERENCES avionics_catalog_human_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  avionics_model_id_snapshot INTEGER NOT NULL,
  member_role TEXT NOT NULL CHECK (member_role IN ('survivor', 'duplicate')),
  row_identity_sha256 TEXT NOT NULL,
  avionics_manufacturer_id_snapshot INTEGER NOT NULL,
  effective_manufacturer_identity_id_snapshot INTEGER NOT NULL,
  manufacturer_name_snapshot TEXT NOT NULL,
  stored_manufacturer_key_snapshot TEXT NOT NULL,
  model_name_snapshot TEXT NOT NULL,
  stored_model_key_snapshot TEXT NOT NULL,
  canonical_model_key_snapshot TEXT NOT NULL,
  catalog_status_snapshot TEXT NOT NULL CHECK (catalog_status_snapshot = 'unreviewed'),
  manufacturer_identifier_kind_snapshot TEXT,
  manufacturer_identifier_snapshot TEXT,
  normalized_manufacturer_identifier_snapshot TEXT,
  identity_source_url_snapshot TEXT,
  identity_source_title_snapshot TEXT,
  identity_evidence_text_snapshot TEXT,
  identity_evidence_kind_snapshot TEXT NOT NULL,
  identity_confidence_snapshot TEXT,
  catalog_reviewed_at_snapshot TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (authorization_sha256, avionics_model_id_snapshot),
  CHECK (avionics_model_id_snapshot > 0),
  CHECK (length(row_identity_sha256) = 64),
  CHECK (row_identity_sha256 = lower(row_identity_sha256)),
  CHECK (row_identity_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (avionics_manufacturer_id_snapshot > 0),
  CHECK (effective_manufacturer_identity_id_snapshot > 0),
  CHECK (length(trim(manufacturer_name_snapshot)) > 0),
  CHECK (length(trim(stored_manufacturer_key_snapshot)) > 0),
  CHECK (length(trim(model_name_snapshot)) > 0),
  CHECK (length(trim(stored_model_key_snapshot)) > 0),
  CHECK (length(trim(canonical_model_key_snapshot)) > 0),
  CHECK (
    (
      manufacturer_identifier_kind_snapshot IS NULL
      AND manufacturer_identifier_snapshot IS NULL
      AND normalized_manufacturer_identifier_snapshot IS NULL
    )
    OR (
      manufacturer_identifier_kind_snapshot IS NOT NULL
      AND manufacturer_identifier_snapshot IS NOT NULL
      AND length(trim(manufacturer_identifier_snapshot)) > 0
      AND normalized_manufacturer_identifier_snapshot IS NOT NULL
      AND length(trim(normalized_manufacturer_identifier_snapshot)) > 0
    )
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_avionics_human_consolidation_one_survivor
ON avionics_catalog_human_consolidation_members (authorization_sha256)
WHERE member_role = 'survivor';

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_authorizations_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_authorizations
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation authorizations are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_authorizations_preserve
BEFORE DELETE ON avionics_catalog_human_consolidation_authorizations
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation authorization audit is permanent');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_members_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_members
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_human_consolidation_authorizations authorization
  JOIN avionics_models model
    ON model.id = NEW.avionics_model_id_snapshot
  JOIN avionics_manufacturers manufacturer
    ON manufacturer.id = model.avionics_manufacturer_id
  JOIN avionics_manufacturer_effective_memberships manufacturer_identity
    ON manufacturer_identity.avionics_manufacturer_id
      = model.avionics_manufacturer_id
  WHERE authorization.authorization_sha256 = NEW.authorization_sha256
    AND (
      (NEW.member_role = 'survivor'
        AND NEW.avionics_model_id_snapshot
          = authorization.survivor_model_id_snapshot)
      OR
      (NEW.member_role = 'duplicate'
        AND NEW.avionics_model_id_snapshot
          <> authorization.survivor_model_id_snapshot)
    )
    AND (
      SELECT count(*)
      FROM avionics_catalog_human_consolidation_members existing
      WHERE existing.authorization_sha256 = NEW.authorization_sha256
    ) < authorization.expected_member_count
    AND NEW.effective_manufacturer_identity_id_snapshot
      = authorization.effective_manufacturer_identity_id_snapshot
    AND (
      NEW.member_role <> 'survivor'
      OR NEW.canonical_model_key_snapshot
        = authorization.canonical_model_key_snapshot
    )
    AND model.catalog_status = 'unreviewed'
    AND NEW.avionics_manufacturer_id_snapshot
      = model.avionics_manufacturer_id
    AND NEW.effective_manufacturer_identity_id_snapshot
      = manufacturer_identity.avionics_manufacturer_identity_id
    AND NEW.manufacturer_name_snapshot = manufacturer.name
    AND NEW.stored_manufacturer_key_snapshot = manufacturer.normalized_name
    AND NEW.model_name_snapshot = model.name
    AND NEW.stored_model_key_snapshot = model.normalized_name
    AND NEW.canonical_model_key_snapshot = model.normalized_name
    AND NEW.catalog_status_snapshot = model.catalog_status
    AND NEW.manufacturer_identifier_kind_snapshot
      IS model.manufacturer_identifier_kind
    AND NEW.manufacturer_identifier_snapshot IS model.manufacturer_identifier
    AND NEW.normalized_manufacturer_identifier_snapshot
      IS model.normalized_manufacturer_identifier
    AND NEW.identity_source_url_snapshot IS model.identity_source_url
    AND NEW.identity_source_title_snapshot IS model.identity_source_title
    AND NEW.identity_evidence_text_snapshot IS model.identity_evidence_text
    AND NEW.identity_evidence_kind_snapshot = model.identity_evidence_kind
    AND NEW.identity_confidence_snapshot IS model.identity_confidence
    AND NEW.catalog_reviewed_at_snapshot IS model.catalog_reviewed_at
)
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation member is not an exact current row snapshot');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_members_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_members
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation member snapshots are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_members_preserve
BEFORE DELETE ON avionics_catalog_human_consolidation_members
BEGIN
  SELECT RAISE(ABORT, 'human avionics consolidation member audit is permanent');
END;

-- This view is the database-enforced stale-state boundary. Any future drift
-- in an authorized identity row removes the pair from this view.
CREATE VIEW IF NOT EXISTS
  avionics_catalog_valid_human_consolidation_pairs AS
SELECT
  authorization.authorization_sha256,
  duplicate_member.avionics_model_id_snapshot AS duplicate_model_id,
  authorization.survivor_model_id_snapshot AS survivor_model_id
FROM avionics_catalog_human_consolidation_authorizations authorization
JOIN avionics_catalog_human_consolidation_members duplicate_member
  ON duplicate_member.authorization_sha256 = authorization.authorization_sha256
 AND duplicate_member.member_role = 'duplicate'
WHERE (
    SELECT count(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization.authorization_sha256
  ) = authorization.expected_member_count
  AND (
    SELECT count(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization.authorization_sha256
      AND member.member_role = 'survivor'
      AND member.avionics_model_id_snapshot
        = authorization.survivor_model_id_snapshot
      AND member.canonical_model_key_snapshot
        = authorization.canonical_model_key_snapshot
  ) = 1
  AND (
    SELECT count(*)
    FROM avionics_catalog_human_consolidation_members member
    WHERE member.authorization_sha256 = authorization.authorization_sha256
      AND member.member_role = 'duplicate'
  ) = authorization.expected_member_count - 1
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_human_consolidation_members member
    LEFT JOIN avionics_models model
      ON model.id = member.avionics_model_id_snapshot
    LEFT JOIN avionics_manufacturers manufacturer
      ON manufacturer.id = model.avionics_manufacturer_id
    LEFT JOIN avionics_manufacturer_effective_memberships manufacturer_identity
      ON manufacturer_identity.avionics_manufacturer_id
        = model.avionics_manufacturer_id
    WHERE member.authorization_sha256 = authorization.authorization_sha256
      AND (
        model.id IS NULL
        OR model.catalog_status <> 'unreviewed'
        OR member.avionics_manufacturer_id_snapshot
          <> model.avionics_manufacturer_id
        OR member.effective_manufacturer_identity_id_snapshot
          IS NOT manufacturer_identity.avionics_manufacturer_identity_id
        OR member.effective_manufacturer_identity_id_snapshot
          <> authorization.effective_manufacturer_identity_id_snapshot
        OR member.manufacturer_name_snapshot <> manufacturer.name
        OR member.stored_manufacturer_key_snapshot
          <> manufacturer.normalized_name
        OR member.model_name_snapshot <> model.name
        OR member.stored_model_key_snapshot <> model.normalized_name
        OR member.canonical_model_key_snapshot <> model.normalized_name
        OR member.catalog_status_snapshot <> model.catalog_status
        OR member.manufacturer_identifier_kind_snapshot
          IS NOT model.manufacturer_identifier_kind
        OR member.manufacturer_identifier_snapshot
          IS NOT model.manufacturer_identifier
        OR member.normalized_manufacturer_identifier_snapshot
          IS NOT model.normalized_manufacturer_identifier
        OR member.identity_source_url_snapshot IS NOT model.identity_source_url
        OR member.identity_source_title_snapshot
          IS NOT model.identity_source_title
        OR member.identity_evidence_text_snapshot
          IS NOT model.identity_evidence_text
        OR member.identity_evidence_kind_snapshot
          <> model.identity_evidence_kind
        OR member.identity_confidence_snapshot IS NOT model.identity_confidence
        OR member.catalog_reviewed_at_snapshot IS NOT model.catalog_reviewed_at
      )
  )
  AND (
    SELECT count(*)
    FROM avionics_models current_model
    JOIN avionics_manufacturer_effective_memberships current_identity
      ON current_identity.avionics_manufacturer_id
        = current_model.avionics_manufacturer_id
    WHERE current_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id_snapshot
      AND EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members selected_member
        WHERE selected_member.authorization_sha256
            = authorization.authorization_sha256
          AND selected_member.canonical_model_key_snapshot
            = current_model.normalized_name
      )
  ) = authorization.expected_member_count
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models current_model
    JOIN avionics_manufacturer_effective_memberships current_identity
      ON current_identity.avionics_manufacturer_id
        = current_model.avionics_manufacturer_id
    WHERE current_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id_snapshot
      AND EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members selected_member
        WHERE selected_member.authorization_sha256
            = authorization.authorization_sha256
          AND selected_member.canonical_model_key_snapshot
            = current_model.normalized_name
      )
      AND NOT EXISTS (
        SELECT 1
        FROM avionics_catalog_human_consolidation_members member
        WHERE member.authorization_sha256 = authorization.authorization_sha256
          AND member.avionics_model_id_snapshot = current_model.id
      )
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_human_consolidation_members left_member
    JOIN avionics_models left_model
      ON left_model.id = left_member.avionics_model_id_snapshot
    JOIN avionics_catalog_human_consolidation_members right_member
      ON right_member.authorization_sha256 = left_member.authorization_sha256
     AND right_member.avionics_model_id_snapshot
       > left_member.avionics_model_id_snapshot
    JOIN avionics_models right_model
      ON right_model.id = right_member.avionics_model_id_snapshot
    WHERE left_member.authorization_sha256 = authorization.authorization_sha256
      AND left_model.manufacturer_identifier_kind IS NOT NULL
      AND left_model.normalized_manufacturer_identifier IS NOT NULL
      AND right_model.manufacturer_identifier_kind IS NOT NULL
      AND right_model.normalized_manufacturer_identifier IS NOT NULL
      AND (
        left_model.manufacturer_identifier_kind
          <> right_model.manufacturer_identifier_kind
        OR left_model.normalized_manufacturer_identifier
          <> right_model.normalized_manufacturer_identifier
      )
  );

CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_guard (
  duplicate_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  authorization_sha256 TEXT NOT NULL
    REFERENCES avionics_catalog_human_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);

-- One claim activates a fully validated set of per-pair guards. The claim is
-- inserted only after every current row snapshot and every required guard have
-- been checked. It remains valid while duplicates are deleted one-by-one, then
-- the consolidation transaction deletes it before commit.
CREATE TABLE IF NOT EXISTS avionics_catalog_human_consolidation_claim (
  authorization_sha256 TEXT PRIMARY KEY
    REFERENCES avionics_catalog_human_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_guard
WHEN EXISTS (
    SELECT 1
    FROM avionics_catalog_human_consolidation_claim claim
    WHERE claim.authorization_sha256 = NEW.authorization_sha256
  )
  OR NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_valid_human_consolidation_pairs valid
    WHERE valid.authorization_sha256 = NEW.authorization_sha256
      AND valid.duplicate_model_id = NEW.duplicate_model_id
      AND valid.survivor_model_id = NEW.survivor_model_id
  )
BEGIN
  SELECT RAISE(ABORT, 'human consolidation guard requires a complete current authorization');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_guard
BEGIN
  SELECT RAISE(ABORT, 'human consolidation guard pairs are immutable');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_claim_validate_insert
BEFORE INSERT ON avionics_catalog_human_consolidation_claim
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_human_consolidation_authorizations authorization
  WHERE authorization.authorization_sha256 = NEW.authorization_sha256
    AND authorization.survivor_model_id_snapshot = NEW.survivor_model_id
    AND (
      SELECT count(*)
      FROM avionics_catalog_human_consolidation_guard guard
      WHERE guard.authorization_sha256 = NEW.authorization_sha256
        AND guard.survivor_model_id = NEW.survivor_model_id
    ) = authorization.expected_member_count - 1
    AND NOT EXISTS (
      SELECT 1
      FROM avionics_catalog_valid_human_consolidation_pairs required_pair
      WHERE required_pair.authorization_sha256 = NEW.authorization_sha256
        AND NOT EXISTS (
          SELECT 1
          FROM avionics_catalog_human_consolidation_guard guard
          WHERE guard.authorization_sha256 = required_pair.authorization_sha256
            AND guard.duplicate_model_id = required_pair.duplicate_model_id
            AND guard.survivor_model_id = required_pair.survivor_model_id
        )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'human consolidation claim requires every complete current guard pair');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_human_consolidation_claim_immutable
BEFORE UPDATE ON avionics_catalog_human_consolidation_claim
BEGIN
  SELECT RAISE(ABORT, 'active human consolidation claims are immutable');
END;

-- Grounded exact-model authority is transaction-scoped. The header binds the
-- two reviewed fingerprints to one complete current manufacturer/model group;
-- pair rows enumerate every duplicate, and only a validated final claim makes
-- them visible to remap triggers. None of these rows may survive commit.
CREATE TABLE IF NOT EXISTS
  avionics_catalog_grounded_consolidation_authorizations (
  authorization_sha256 TEXT PRIMARY KEY,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  effective_manufacturer_identity_id INTEGER NOT NULL
    REFERENCES avionics_manufacturer_identities(id) ON DELETE RESTRICT,
  normalized_model_key TEXT NOT NULL,
  expected_member_count INTEGER NOT NULL CHECK (expected_member_count >= 2),
  reviewed_catalog_fingerprint TEXT NOT NULL,
  manufacturer_collision_snapshot_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(authorization_sha256) = 64),
  CHECK (authorization_sha256 = lower(authorization_sha256)),
  CHECK (authorization_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(trim(normalized_model_key)) > 0),
  CHECK (length(reviewed_catalog_fingerprint) = 64),
  CHECK (reviewed_catalog_fingerprint = lower(reviewed_catalog_fingerprint)),
  CHECK (reviewed_catalog_fingerprint NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(manufacturer_collision_snapshot_sha256) = 64),
  CHECK (manufacturer_collision_snapshot_sha256 =
         lower(manufacturer_collision_snapshot_sha256)),
  CHECK (manufacturer_collision_snapshot_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_grounded_consolidation_authorization_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_authorizations
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models survivor
  JOIN avionics_manufacturer_effective_memberships survivor_identity
    ON survivor_identity.avionics_manufacturer_id
      = survivor.avionics_manufacturer_id
  WHERE survivor.id = NEW.survivor_model_id
    AND survivor.catalog_status = 'unreviewed'
    AND survivor_identity.avionics_manufacturer_identity_id
      = NEW.effective_manufacturer_identity_id
    AND survivor.normalized_name = NEW.normalized_model_key
    AND (
      SELECT count(*)
      FROM avionics_models member
      JOIN avionics_manufacturer_effective_memberships member_identity
        ON member_identity.avionics_manufacturer_id
          = member.avionics_manufacturer_id
      WHERE member_identity.avionics_manufacturer_identity_id
          = NEW.effective_manufacturer_identity_id
        AND member.normalized_name = NEW.normalized_model_key
    ) = NEW.expected_member_count
)
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation authorization requires the complete current exact-model group');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_grounded_consolidation_authorization_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_authorizations
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation authorizations are immutable');
END;

CREATE TABLE IF NOT EXISTS avionics_catalog_grounded_consolidation_guard (
  duplicate_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  authorization_sha256 TEXT NOT NULL
    REFERENCES avionics_catalog_grounded_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);

CREATE TABLE IF NOT EXISTS avionics_catalog_grounded_consolidation_claim (
  authorization_sha256 TEXT PRIMARY KEY
    REFERENCES avionics_catalog_grounded_consolidation_authorizations(
      authorization_sha256
    ) ON DELETE RESTRICT,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_grounded_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_guard
WHEN EXISTS (
    SELECT 1
    FROM avionics_catalog_grounded_consolidation_claim claim
    WHERE claim.authorization_sha256 = NEW.authorization_sha256
  )
  OR NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_grounded_consolidation_authorizations authorization
    JOIN avionics_models duplicate ON duplicate.id = NEW.duplicate_model_id
    JOIN avionics_models survivor ON survivor.id = NEW.survivor_model_id
    JOIN avionics_manufacturer_effective_memberships duplicate_identity
      ON duplicate_identity.avionics_manufacturer_id
        = duplicate.avionics_manufacturer_id
    JOIN avionics_manufacturer_effective_memberships survivor_identity
      ON survivor_identity.avionics_manufacturer_id
        = survivor.avionics_manufacturer_id
    WHERE authorization.authorization_sha256 = NEW.authorization_sha256
      AND authorization.survivor_model_id = NEW.survivor_model_id
      AND duplicate.catalog_status = 'unreviewed'
      AND survivor.catalog_status = 'unreviewed'
      AND duplicate_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id
      AND survivor_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id
      AND duplicate.normalized_name = authorization.normalized_model_key
      AND survivor.normalized_name = authorization.normalized_model_key
      AND (
        duplicate.manufacturer_identifier_kind IS NULL
        OR survivor.manufacturer_identifier_kind IS NULL
        OR (
          duplicate.manufacturer_identifier_kind
            = survivor.manufacturer_identifier_kind
          AND lower(replace(replace(replace(replace(replace(
            trim(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
            = lower(replace(replace(replace(replace(replace(
              trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
        )
      )
      AND (
        SELECT count(*)
        FROM avionics_catalog_grounded_consolidation_guard existing
        WHERE existing.authorization_sha256 = NEW.authorization_sha256
      ) < authorization.expected_member_count - 1
  )
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation guard requires an inactive complete-group authorization and an exact current pair');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_grounded_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_guard
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation guard pairs are immutable');
END;

CREATE VIEW IF NOT EXISTS avionics_catalog_valid_grounded_consolidation_pairs AS
SELECT guard.authorization_sha256,
       guard.duplicate_model_id,
       guard.survivor_model_id
FROM avionics_catalog_grounded_consolidation_guard guard
JOIN avionics_catalog_grounded_consolidation_authorizations authorization
  ON authorization.authorization_sha256 = guard.authorization_sha256
WHERE (
    SELECT count(*)
    FROM avionics_catalog_grounded_consolidation_guard sibling
    WHERE sibling.authorization_sha256 = authorization.authorization_sha256
      AND sibling.survivor_model_id = authorization.survivor_model_id
  ) = authorization.expected_member_count - 1
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models member
    JOIN avionics_manufacturer_effective_memberships member_identity
      ON member_identity.avionics_manufacturer_id
        = member.avionics_manufacturer_id
    WHERE member_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id
      AND member.normalized_name = authorization.normalized_model_key
      AND (
        member.catalog_status <> 'unreviewed'
        OR (
          member.id <> authorization.survivor_model_id
          AND NOT EXISTS (
            SELECT 1
            FROM avionics_catalog_grounded_consolidation_guard required_guard
            WHERE required_guard.authorization_sha256
                = authorization.authorization_sha256
              AND required_guard.duplicate_model_id = member.id
              AND required_guard.survivor_model_id
                = authorization.survivor_model_id
          )
        )
      )
  )
  AND (
    SELECT count(*)
    FROM avionics_models member
    JOIN avionics_manufacturer_effective_memberships member_identity
      ON member_identity.avionics_manufacturer_id
        = member.avionics_manufacturer_id
    WHERE member_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id
      AND member.normalized_name = authorization.normalized_model_key
  ) = authorization.expected_member_count
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models left_model
    JOIN avionics_manufacturer_effective_memberships left_identity
      ON left_identity.avionics_manufacturer_id
        = left_model.avionics_manufacturer_id
    JOIN avionics_models right_model ON right_model.id > left_model.id
    JOIN avionics_manufacturer_effective_memberships right_identity
      ON right_identity.avionics_manufacturer_id
        = right_model.avionics_manufacturer_id
     AND right_identity.avionics_manufacturer_identity_id
        = left_identity.avionics_manufacturer_identity_id
    WHERE left_identity.avionics_manufacturer_identity_id
        = authorization.effective_manufacturer_identity_id
      AND left_model.normalized_name = authorization.normalized_model_key
      AND right_model.normalized_name = authorization.normalized_model_key
      AND left_model.manufacturer_identifier_kind IS NOT NULL
      AND right_model.manufacturer_identifier_kind IS NOT NULL
      AND (
        left_model.manufacturer_identifier_kind
          <> right_model.manufacturer_identifier_kind
        OR lower(replace(replace(replace(replace(replace(
          trim(left_model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
          <> lower(replace(replace(replace(replace(replace(
            trim(right_model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      )
  );

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_grounded_consolidation_claim_validate_insert
BEFORE INSERT ON avionics_catalog_grounded_consolidation_claim
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_grounded_consolidation_authorizations authorization
  WHERE authorization.authorization_sha256 = NEW.authorization_sha256
    AND authorization.survivor_model_id = NEW.survivor_model_id
    AND (
      SELECT count(*)
      FROM avionics_catalog_valid_grounded_consolidation_pairs valid
      WHERE valid.authorization_sha256 = NEW.authorization_sha256
        AND valid.survivor_model_id = NEW.survivor_model_id
    ) = authorization.expected_member_count - 1
)
BEGIN
  SELECT RAISE(ABORT, 'grounded consolidation claim requires every member of the complete current exact-model group');
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_catalog_grounded_consolidation_claim_immutable
BEFORE UPDATE ON avionics_catalog_grounded_consolidation_claim
BEGIN
  SELECT RAISE(ABORT, 'active grounded consolidation claims are immutable');
END;

-- The stable-identifier guard remains separate: a grounded exact-name review
-- can never be authorized by inserting one ordinary pair directly.
CREATE TABLE IF NOT EXISTS avionics_catalog_consolidation_guard (
  duplicate_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  survivor_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE RESTRICT,
  purpose TEXT NOT NULL DEFAULT 'legacy_identity_consolidation'
    CHECK (purpose = 'legacy_identity_consolidation'),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (duplicate_model_id <> survivor_model_id)
);

CREATE TRIGGER IF NOT EXISTS avionics_catalog_consolidation_guard_validate_insert
BEFORE INSERT ON avionics_catalog_consolidation_guard
WHEN NOT (
  NEW.purpose = 'legacy_identity_consolidation'
  AND EXISTS (
  SELECT 1
  FROM avionics_models duplicate
  JOIN avionics_models survivor ON survivor.id = NEW.survivor_model_id
  WHERE duplicate.id = NEW.duplicate_model_id
    AND duplicate.catalog_status IN ('unreviewed', 'approved')
    AND survivor.catalog_status IN ('unreviewed', 'approved')
    AND (
      survivor.catalog_status = 'approved'
      OR duplicate.catalog_status = 'unreviewed'
    )
    AND EXISTS (
        SELECT 1
        FROM avionics_manufacturer_effective_memberships duplicate_identity
        JOIN avionics_manufacturer_effective_memberships survivor_identity
          ON survivor_identity.avionics_manufacturer_id
            = survivor.avionics_manufacturer_id
        WHERE duplicate_identity.avionics_manufacturer_id
            = duplicate.avionics_manufacturer_id
          AND (
            duplicate_identity.avionics_manufacturer_identity_id
              = survivor_identity.avionics_manufacturer_identity_id
            OR EXISTS (
              SELECT 1
              FROM avionics_manufacturer_alias_candidates candidate
              JOIN avionics_manufacturer_effective_memberships source_identity
                ON source_identity.avionics_manufacturer_id
                  = candidate.avionics_manufacturer_id
              JOIN avionics_manufacturer_effective_identities target_identity
                ON target_identity.identity_id
                  = candidate.candidate_manufacturer_identity_id
              WHERE candidate.review_status = 'approved'
                AND candidate.decision_evidence_source_url IS NOT NULL
                AND length(trim(candidate.decision_evidence_source_url)) > 0
                AND candidate.decision_evidence_source_title IS NOT NULL
                AND length(trim(candidate.decision_evidence_source_title)) > 0
                AND candidate.decision_evidence_text IS NOT NULL
                AND length(trim(candidate.decision_evidence_text)) > 0
                AND candidate.reviewed_by_user_id IS NOT NULL
                AND candidate.reviewed_at IS NOT NULL
                AND (
                  (
                    source_identity.avionics_manufacturer_identity_id
                      = duplicate_identity.avionics_manufacturer_identity_id
                    AND target_identity.avionics_manufacturer_identity_id
                      = survivor_identity.avionics_manufacturer_identity_id
                  )
                  OR (
                    source_identity.avionics_manufacturer_identity_id
                      = survivor_identity.avionics_manufacturer_identity_id
                    AND target_identity.avionics_manufacturer_identity_id
                      = duplicate_identity.avionics_manufacturer_identity_id
                  )
                )
            )
          )
    )
    AND duplicate.manufacturer_identifier_kind IS NOT NULL
    AND duplicate.manufacturer_identifier_kind
      = survivor.manufacturer_identifier_kind
    AND duplicate.manufacturer_identifier IS NOT NULL
    AND length(trim(duplicate.manufacturer_identifier)) > 0
    AND duplicate.normalized_manufacturer_identifier IS NOT NULL
    AND length(trim(duplicate.normalized_manufacturer_identifier)) > 0
    AND survivor.manufacturer_identifier IS NOT NULL
    AND length(trim(survivor.manufacturer_identifier)) > 0
    AND survivor.normalized_manufacturer_identifier IS NOT NULL
    AND length(trim(survivor.normalized_manufacturer_identifier)) > 0
    AND lower(replace(replace(replace(replace(replace(
      trim(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = lower(replace(replace(replace(replace(replace(
        trim(duplicate.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    AND lower(replace(replace(replace(replace(replace(
      trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = lower(replace(replace(replace(replace(replace(
        trim(survivor.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    AND lower(replace(replace(replace(replace(replace(
      trim(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = lower(replace(replace(replace(replace(replace(
        trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  )
)
BEGIN
  SELECT RAISE(ABORT, 'consolidation guard pair does not satisfy its declared identity authority');
END;

CREATE TRIGGER IF NOT EXISTS avionics_catalog_consolidation_guard_immutable
BEFORE UPDATE ON avionics_catalog_consolidation_guard
BEGIN
  SELECT RAISE(ABORT, 'consolidation authorization pairs are immutable');
END;

-- Revalidate every authorization at use time as well as at insertion time.
-- This prevents a stale or accidentally committed pair from becoming a broad
-- bypass if either endpoint no longer represents the exact same identity.
CREATE VIEW IF NOT EXISTS avionics_catalog_authorized_consolidations AS
SELECT guard.duplicate_model_id, guard.survivor_model_id
FROM avionics_catalog_consolidation_guard guard
JOIN avionics_models duplicate ON duplicate.id = guard.duplicate_model_id
JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
WHERE guard.purpose = 'legacy_identity_consolidation'
  AND duplicate.catalog_status IN ('unreviewed', 'approved')
  AND survivor.catalog_status IN ('unreviewed', 'approved')
  AND (
    survivor.catalog_status = 'approved'
    OR duplicate.catalog_status = 'unreviewed'
  )
  AND EXISTS (
      SELECT 1
      FROM avionics_manufacturer_effective_memberships duplicate_identity
      JOIN avionics_manufacturer_effective_memberships survivor_identity
        ON survivor_identity.avionics_manufacturer_id
          = survivor.avionics_manufacturer_id
      WHERE duplicate_identity.avionics_manufacturer_id
          = duplicate.avionics_manufacturer_id
        AND (
          duplicate_identity.avionics_manufacturer_identity_id
            = survivor_identity.avionics_manufacturer_identity_id
          OR EXISTS (
            SELECT 1
            FROM avionics_manufacturer_alias_candidates candidate
            JOIN avionics_manufacturer_effective_memberships source_identity
              ON source_identity.avionics_manufacturer_id
                = candidate.avionics_manufacturer_id
            JOIN avionics_manufacturer_effective_identities target_identity
              ON target_identity.identity_id
                = candidate.candidate_manufacturer_identity_id
            WHERE candidate.review_status = 'approved'
              AND candidate.decision_evidence_source_url IS NOT NULL
              AND length(trim(candidate.decision_evidence_source_url)) > 0
              AND candidate.decision_evidence_source_title IS NOT NULL
              AND length(trim(candidate.decision_evidence_source_title)) > 0
              AND candidate.decision_evidence_text IS NOT NULL
              AND length(trim(candidate.decision_evidence_text)) > 0
              AND candidate.reviewed_by_user_id IS NOT NULL
              AND candidate.reviewed_at IS NOT NULL
              AND (
                (
                  source_identity.avionics_manufacturer_identity_id
                    = duplicate_identity.avionics_manufacturer_identity_id
                  AND target_identity.avionics_manufacturer_identity_id
                    = survivor_identity.avionics_manufacturer_identity_id
                )
                OR (
                  source_identity.avionics_manufacturer_identity_id
                    = survivor_identity.avionics_manufacturer_identity_id
                  AND target_identity.avionics_manufacturer_identity_id
                    = duplicate_identity.avionics_manufacturer_identity_id
                )
              )
          )
        )
  )
  AND duplicate.manufacturer_identifier_kind IS NOT NULL
  AND duplicate.manufacturer_identifier_kind
    = survivor.manufacturer_identifier_kind
  AND duplicate.manufacturer_identifier IS NOT NULL
  AND length(trim(duplicate.manufacturer_identifier)) > 0
  AND duplicate.normalized_manufacturer_identifier IS NOT NULL
  AND length(trim(duplicate.normalized_manufacturer_identifier)) > 0
  AND survivor.manufacturer_identifier IS NOT NULL
  AND length(trim(survivor.manufacturer_identifier)) > 0
  AND survivor.normalized_manufacturer_identifier IS NOT NULL
  AND length(trim(survivor.normalized_manufacturer_identifier)) > 0
  AND lower(replace(replace(replace(replace(replace(
    trim(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = lower(replace(replace(replace(replace(replace(
      trim(duplicate.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  AND lower(replace(replace(replace(replace(replace(
    trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = lower(replace(replace(replace(replace(replace(
      trim(survivor.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  AND lower(replace(replace(replace(replace(replace(
    trim(duplicate.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    = lower(replace(replace(replace(replace(replace(
      trim(survivor.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
UNION ALL
SELECT grounded_guard.duplicate_model_id, grounded_guard.survivor_model_id
FROM avionics_catalog_grounded_consolidation_guard grounded_guard
JOIN avionics_catalog_grounded_consolidation_claim claim
  ON claim.authorization_sha256 = grounded_guard.authorization_sha256
 AND claim.survivor_model_id = grounded_guard.survivor_model_id
UNION ALL
SELECT human_guard.duplicate_model_id, human_guard.survivor_model_id
FROM avionics_catalog_human_consolidation_guard human_guard
JOIN avionics_catalog_human_consolidation_claim claim
 ON claim.authorization_sha256 = human_guard.authorization_sha256
 AND claim.survivor_model_id = human_guard.survivor_model_id;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260810_avionics_grounded_exact_model_consolidation',
  1,
  '36f9ff06bf42fc769508ecfe578f4b4a11f2e0072b81efebed1dee8958654f2a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

CREATE TRIGGER IF NOT EXISTS avionics_models_consolidation_identity_immutable
BEFORE UPDATE OF catalog_status, avionics_manufacturer_id, name,
  normalized_name, manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier
ON avionics_models
WHEN EXISTS (
  SELECT 1 FROM avionics_catalog_consolidation_guard guard
  WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
  UNION ALL
  SELECT 1 FROM avionics_catalog_grounded_consolidation_guard grounded_guard
  WHERE grounded_guard.duplicate_model_id = OLD.id
     OR grounded_guard.survivor_model_id = OLD.id
  UNION ALL
  SELECT 1 FROM avionics_catalog_human_consolidation_guard guard
  WHERE guard.duplicate_model_id = OLD.id OR guard.survivor_model_id = OLD.id
)
BEGIN
  SELECT RAISE(ABORT, 'guarded avionics consolidation identities are immutable');
END;

CREATE TRIGGER IF NOT EXISTS avionics_approved_identity_validate_insert
BEFORE INSERT ON avionics_approved_product_identities
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  JOIN avionics_manufacturer_effective_memberships manufacturer_identity
    ON manufacturer_identity.avionics_manufacturer_id
      = model.avionics_manufacturer_id
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
    AND manufacturer_identity.avionics_manufacturer_identity_id
      = NEW.avionics_manufacturer_identity_id
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_product_key
    AND model.manufacturer_identifier_kind = NEW.manufacturer_identifier_kind
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_identifier_key
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics identity must match its catalog product');
END;

CREATE TRIGGER IF NOT EXISTS avionics_approved_identity_validate_update
BEFORE UPDATE ON avionics_approved_product_identities
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  JOIN avionics_manufacturer_effective_memberships manufacturer_identity
    ON manufacturer_identity.avionics_manufacturer_id
      = model.avionics_manufacturer_id
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
    AND manufacturer_identity.avionics_manufacturer_identity_id
      = NEW.avionics_manufacturer_identity_id
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_product_key
    AND model.manufacturer_identifier_kind = NEW.manufacturer_identifier_kind
    AND lower(replace(replace(replace(replace(replace(
      trim(model.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      = NEW.canonical_identifier_key
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics identity must match its catalog product');
END;

CREATE TRIGGER IF NOT EXISTS avionics_approved_identity_preserve_delete
BEFORE DELETE ON avionics_approved_product_identities
WHEN EXISTS (
  SELECT 1 FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_authorized_consolidations authorization
  JOIN avionics_models survivor
    ON survivor.id = authorization.survivor_model_id
  WHERE authorization.duplicate_model_id = OLD.avionics_model_id
    AND survivor.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product must retain its canonical identity');
END;

CREATE TRIGGER IF NOT EXISTS avionics_models_canonical_identity_validate_update
BEFORE UPDATE OF catalog_status, avionics_manufacturer_id,
  normalized_name, normalized_manufacturer_identifier
ON avionics_models
WHEN NEW.catalog_status = 'approved'
  AND (
    NOT EXISTS (
      SELECT 1
      FROM avionics_manufacturer_effective_memberships manufacturer_identity
      WHERE manufacturer_identity.avionics_manufacturer_id
        = NEW.avionics_manufacturer_id
    )
    OR length(lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))) = 0
    OR length(lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))) = 0
    OR NEW.name GLOB '*[^A-Za-z0-9 ./_-]*'
    OR NEW.normalized_name GLOB '*[^A-Za-z0-9 ./_-]*'
    OR lower(replace(replace(replace(replace(replace(
      trim(NEW.name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      <> lower(replace(replace(replace(replace(replace(
        trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
    OR NEW.manufacturer_identifier IS NULL
    OR NEW.manufacturer_identifier GLOB '*[^A-Za-z0-9 ./_-]*'
    OR NEW.normalized_manufacturer_identifier
      GLOB '*[^A-Za-z0-9 ./_-]*'
    OR lower(replace(replace(replace(replace(replace(
      trim(NEW.manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
      <> lower(replace(replace(replace(replace(replace(
        trim(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  )
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product requires deterministic canonical identity keys');
END;

CREATE TRIGGER IF NOT EXISTS avionics_models_canonical_identity_sync_update
AFTER UPDATE OF catalog_status, avionics_manufacturer_id,
  normalized_name, normalized_manufacturer_identifier
ON avionics_models
WHEN NEW.catalog_status = 'approved'
BEGIN
  INSERT INTO avionics_approved_product_identities (
    avionics_model_id,
    avionics_manufacturer_identity_id,
    canonical_product_key,
    manufacturer_identifier_kind,
    canonical_identifier_key
  )
  SELECT
    NEW.id,
    manufacturer_identity.avionics_manufacturer_identity_id,
    lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_name), ' ', ''), '-', ''), '/', ''), '.', ''), '_', '')),
    NEW.manufacturer_identifier_kind,
    lower(replace(replace(replace(replace(replace(
      trim(NEW.normalized_manufacturer_identifier), ' ', ''), '-', ''), '/', ''), '.', ''), '_', ''))
  FROM avionics_manufacturer_effective_memberships manufacturer_identity
  WHERE manufacturer_identity.avionics_manufacturer_id
    = NEW.avionics_manufacturer_id
  ON CONFLICT (avionics_model_id) DO UPDATE SET
    avionics_manufacturer_identity_id
      = excluded.avionics_manufacturer_identity_id,
    canonical_product_key = excluded.canonical_product_key,
    manufacturer_identifier_kind = excluded.manufacturer_identifier_kind,
    canonical_identifier_key = excluded.canonical_identifier_key,
    updated_at = CURRENT_TIMESTAMP;
END;

CREATE TRIGGER IF NOT EXISTS avionics_models_approved_identity_immutable
BEFORE UPDATE ON avionics_models
WHEN OLD.catalog_status = 'approved'
AND (
  NEW.catalog_status IS NOT OLD.catalog_status
  OR NEW.avionics_manufacturer_id IS NOT OLD.avionics_manufacturer_id
  OR NEW.name IS NOT OLD.name
  OR NEW.normalized_name IS NOT OLD.normalized_name
  OR NEW.manufacturer_identifier_kind IS NOT OLD.manufacturer_identifier_kind
  OR NEW.manufacturer_identifier IS NOT OLD.manufacturer_identifier
  OR NEW.normalized_manufacturer_identifier
    IS NOT OLD.normalized_manufacturer_identifier
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product cannot be demoted or rewrite canonical identity');
END;

CREATE TRIGGER IF NOT EXISTS avionics_models_approved_delete_guard
BEFORE DELETE ON avionics_models
WHEN OLD.catalog_status = 'approved'
AND NOT EXISTS (
  SELECT 1
  FROM avionics_catalog_authorized_consolidations authorization
  JOIN avionics_models survivor
    ON survivor.id = authorization.survivor_model_id
  WHERE authorization.duplicate_model_id = OLD.id
    AND survivor.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics product deletion requires exact consolidation authorization');
END;

CREATE TABLE IF NOT EXISTS avionics_model_types (
  avionics_model_id INTEGER NOT NULL
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_type_id INTEGER NOT NULL
    REFERENCES avionics_types(id) ON DELETE RESTRICT,
  PRIMARY KEY (avionics_model_id, avionics_type_id)
);

CREATE INDEX IF NOT EXISTS idx_avionics_model_types_type
  ON avionics_model_types (avionics_type_id, avionics_model_id);

-- Approval is staged: create an unreviewed product, attach at least one
-- capability, then approve it. An approved product can never be left typeless.
CREATE TRIGGER IF NOT EXISTS avionics_models_approved_types_insert
BEFORE INSERT ON avionics_models
WHEN NEW.catalog_status = 'approved'
BEGIN
  SELECT RAISE(ABORT, 'avionics approval must be staged from an unreviewed product');
END;

CREATE TRIGGER IF NOT EXISTS avionics_models_approved_types_update
BEFORE UPDATE OF catalog_status ON avionics_models
WHEN NEW.catalog_status = 'approved'
AND NOT EXISTS (
  SELECT 1
  FROM avionics_model_types membership
  WHERE membership.avionics_model_id = NEW.id
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics model requires at least one type');
END;

CREATE TRIGGER IF NOT EXISTS avionics_model_types_preserve_approved_delete
BEFORE DELETE ON avionics_model_types
WHEN EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id
    AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_model_types other
  WHERE other.avionics_model_id = OLD.avionics_model_id
    AND other.avionics_type_id <> OLD.avionics_type_id
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics model cannot lose its last type');
END;

CREATE TRIGGER IF NOT EXISTS avionics_model_types_preserve_approved_update
BEFORE UPDATE OF avionics_model_id ON avionics_model_types
WHEN NEW.avionics_model_id <> OLD.avionics_model_id
AND EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = OLD.avionics_model_id
    AND model.catalog_status = 'approved'
)
AND NOT EXISTS (
  SELECT 1
  FROM avionics_model_types other
  WHERE other.avionics_model_id = OLD.avionics_model_id
    AND other.avionics_type_id <> OLD.avionics_type_id
)
BEGIN
  SELECT RAISE(ABORT, 'approved avionics model cannot lose its last type');
END;

CREATE TABLE IF NOT EXISTS avionics_suite_components (
  suite_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  component_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE CASCADE,
  quantity INTEGER NOT NULL DEFAULT 1 CHECK (quantity > 0),
  PRIMARY KEY (suite_model_id, component_model_id),
  CHECK (suite_model_id <> component_model_id)
);

CREATE TRIGGER IF NOT EXISTS avionics_suite_components_approved_insert
BEFORE INSERT ON avionics_suite_components
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models suite_model
  WHERE suite_model.id = NEW.suite_model_id
    AND suite_model.catalog_status = 'approved'
)
OR NOT EXISTS (
  SELECT 1
  FROM avionics_models component_model
  WHERE component_model.id = NEW.component_model_id
    AND component_model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(ABORT, 'avionics suite membership requires approved catalog entries');
END;

CREATE TRIGGER IF NOT EXISTS avionics_suite_components_approved_update
BEFORE UPDATE ON avionics_suite_components
WHEN (
  NEW.suite_model_id IS NOT OLD.suite_model_id
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models suite_model
    WHERE suite_model.id = NEW.suite_model_id
      AND suite_model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = OLD.suite_model_id
    WHERE guard.duplicate_model_id = OLD.suite_model_id
      AND guard.survivor_model_id = NEW.suite_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
  )
)
OR (
  NEW.component_model_id IS NOT OLD.component_model_id
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models component_model
    WHERE component_model.id = NEW.component_model_id
      AND component_model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = OLD.component_model_id
    WHERE guard.duplicate_model_id = OLD.component_model_id
      AND guard.survivor_model_id = NEW.component_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'avionics suite membership requires approved catalog entries');
END;

CREATE TABLE IF NOT EXISTS aircraft_sale_listings (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  is_verified INTEGER NOT NULL DEFAULT 0 CHECK (is_verified IN (0, 1)),
  source_url TEXT,
  model_year INTEGER NOT NULL,
  asking_price_usd REAL NOT NULL,
  currency TEXT NOT NULL DEFAULT 'USD',
  added_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  status TEXT NOT NULL DEFAULT 'active',
  ingestion_state TEXT NOT NULL DEFAULT 'incomplete'
    CHECK (ingestion_state IN (
      'incomplete', 'pending_review', 'ready', 'quarantined'
    )),
  ingestion_error TEXT,
  ingestion_completed_at TEXT,
  registration_number TEXT,
  serial_number TEXT,
  airframe_hours REAL NOT NULL,
  engine_hours REAL,
  engine_time_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK (engine_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  engine_time_evidence TEXT,
  engine_time_confidence TEXT
    CHECK (engine_time_confidence IS NULL OR engine_time_confidence IN ('high', 'medium', 'low')),
  propeller_hours REAL,
  propeller_time_basis TEXT NOT NULL DEFAULT 'unknown'
    CHECK (propeller_time_basis IN ('SNEW', 'SMOH', 'SFOH', 'SPOH', 'unknown')),
  propeller_time_evidence TEXT,
  propeller_time_confidence TEXT
    CHECK (propeller_time_confidence IS NULL OR propeller_time_confidence IN ('high', 'medium', 'low')),
  installed_engine_model_id INTEGER REFERENCES engine_models(id),
  installed_engine_source_url TEXT,
  installed_engine_evidence_text TEXT,
  installed_engine_confidence TEXT
    CHECK (installed_engine_confidence IS NULL OR installed_engine_confidence IN ('high', 'medium', 'low')),
  installed_propeller_model_id INTEGER REFERENCES propeller_models(id),
  installed_propeller_source_url TEXT,
  installed_propeller_evidence_text TEXT,
  installed_propeller_confidence TEXT
    CHECK (installed_propeller_confidence IS NULL OR installed_propeller_confidence IN ('high', 'medium', 'low')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (source_url IS NOT NULL OR is_verified = 0),
  CHECK (
    ingestion_state = 'quarantined'
    OR asking_price_usd BETWEEN 1000 AND 250000000
  ),
  CHECK (airframe_hours >= 0 AND airframe_hours <= 100000),
  CHECK (engine_hours IS NULL OR (engine_hours >= 0 AND engine_hours <= 100000)),
  CHECK (propeller_hours IS NULL OR (propeller_hours >= 0 AND propeller_hours <= 100000)),
  CHECK (engine_hours IS NOT NULL OR engine_time_basis = 'unknown'),
  CHECK (propeller_hours IS NOT NULL OR propeller_time_basis = 'unknown'),
  CHECK (
    (installed_engine_model_id IS NULL
      AND installed_engine_source_url IS NULL
      AND installed_engine_evidence_text IS NULL
      AND installed_engine_confidence IS NULL)
    OR
    (installed_engine_model_id IS NOT NULL
      AND installed_engine_source_url IS NOT NULL
      AND installed_engine_evidence_text IS NOT NULL
      AND installed_engine_confidence IS NOT NULL)
  ),
  CHECK (
    (installed_propeller_model_id IS NULL
      AND installed_propeller_source_url IS NULL
      AND installed_propeller_evidence_text IS NULL
      AND installed_propeller_confidence IS NULL)
    OR
    (installed_propeller_model_id IS NOT NULL
      AND installed_propeller_source_url IS NOT NULL
      AND installed_propeller_evidence_text IS NOT NULL
      AND installed_propeller_confidence IS NOT NULL)
  ),
  CHECK (
    ingestion_state <> 'ready'
    OR (ingestion_error IS NULL AND ingestion_completed_at IS NOT NULL)
  ),
  CHECK (ingestion_state <> 'quarantined' OR ingestion_error IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_variant
  ON aircraft_sale_listings (aircraft_model_variant_id, is_verified, added_at);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_user
  ON aircraft_sale_listings (created_by_user_id);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listings_ingestion
  ON aircraft_sale_listings (ingestion_state, status, added_at);

CREATE UNIQUE INDEX IF NOT EXISTS uq_aircraft_sale_listings_owner_source
  ON aircraft_sale_listings (created_by_user_id, source_url)
  WHERE source_url IS NOT NULL AND length(trim(source_url)) > 0;

CREATE TABLE IF NOT EXISTS plugin_installs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  public_key_base64 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  revoked_at TEXT
);

CREATE INDEX IF NOT EXISTS idx_plugin_installs_user
  ON plugin_installs (user_id, revoked_at);

CREATE TABLE IF NOT EXISTS plugin_submissions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  user_id INTEGER NOT NULL REFERENCES users(id),
  plugin_install_id INTEGER NOT NULL REFERENCES plugin_installs(id),
  source_url TEXT NOT NULL,
  submitted_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  rendered_html TEXT NOT NULL,
  rendered_html_sha256 TEXT NOT NULL,
  signature_base64 TEXT NOT NULL,
  extracted_listing_json TEXT,
  extraction_error TEXT,
  canonical_listing_id INTEGER REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL
);

CREATE INDEX IF NOT EXISTS idx_plugin_submissions_user
  ON plugin_submissions (user_id, submitted_at);

CREATE INDEX IF NOT EXISTS idx_plugin_submissions_listing
  ON plugin_submissions (canonical_listing_id);

CREATE UNIQUE INDEX IF NOT EXISTS uq_plugin_submissions_signed_capture
  ON plugin_submissions (
    user_id, plugin_install_id, source_url, rendered_html_sha256
  );

-- Durable handoff for listings whose deterministic extraction succeeded but
-- whose catalog-affecting observations still require explicit review.
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_pending_reviews (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER
    REFERENCES plugin_submissions(id) ON DELETE SET NULL,
  extraction_sha256 TEXT NOT NULL,
  catalog_revision_sha256 TEXT NOT NULL,
  pending_aspect_count INTEGER NOT NULL CHECK (pending_aspect_count >= 1),
  review_payload_json TEXT NOT NULL,
  review_payload_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(extraction_sha256) = 64
    AND extraction_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(catalog_revision_sha256) = 64
    AND catalog_revision_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(trim(review_payload_json)) > 0),
  CHECK (length(review_payload_sha256) = 64
    AND review_payload_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listing_pending_reviews_submission
  ON aircraft_sale_listing_pending_reviews (plugin_submission_id);

-- Terminal receipts for current-schema avionics extraction occurrences. A
-- pending occurrence remains represented by the listing review and therefore
-- has no row here. Receipts retain only immutable correlation hashes and the
-- terminal result; listing text and provider research are not duplicated.
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_dispositions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  extraction_sha256 TEXT NOT NULL,
  occurrence_index INTEGER NOT NULL CHECK (occurrence_index >= 0),
  occurrence_role TEXT NOT NULL CHECK (occurrence_role IN ('primary', 'replacement')),
  occurrence_fingerprint TEXT NOT NULL,
  outcome TEXT NOT NULL CHECK (outcome IN ('linked', 'discarded')),
  avionics_model_id INTEGER REFERENCES avionics_models(id) ON DELETE RESTRICT,
  reason_code TEXT NOT NULL CHECK (length(trim(reason_code)) BETWEEN 1 AND 100),
  decision_reason TEXT NOT NULL CHECK (length(trim(decision_reason)) BETWEEN 1 AND 500),
  decision_source TEXT NOT NULL CHECK (decision_source IN ('automatic', 'manual')),
  actor_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  policy_version TEXT NOT NULL CHECK (length(trim(policy_version)) BETWEEN 1 AND 100),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (plugin_submission_id, extraction_sha256, occurrence_index, occurrence_role),
  CHECK (length(extraction_sha256) = 64
    AND extraction_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(occurrence_fingerprint) = 64
    AND occurrence_fingerprint NOT GLOB '*[^0-9a-f]*'),
  CHECK (
    (outcome = 'linked' AND avionics_model_id IS NOT NULL)
    OR (outcome = 'discarded' AND avionics_model_id IS NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_listing_avionics_dispositions_listing
  ON aircraft_sale_listing_avionics_dispositions (aircraft_sale_listing_id, occurrence_index);

CREATE TRIGGER IF NOT EXISTS trg_listing_avionics_dispositions_immutable
BEFORE UPDATE ON aircraft_sale_listing_avionics_dispositions
BEGIN
  SELECT RAISE(ABORT, 'avionics occurrence dispositions are immutable');
END;

-- Durable operational queue for automatic listing verification. These rows
-- store scheduling state and sanitized verifier outcomes only; provider
-- prompts, source dossiers, and grounding evidence are deliberately excluded.
CREATE TABLE IF NOT EXISTS listing_verification_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  owner_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  idempotency_key TEXT NOT NULL,
  request_fingerprint TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'cancelling', 'completed', 'cancelled')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  UNIQUE (owner_user_id, idempotency_key),
  CHECK (length(trim(idempotency_key)) BETWEEN 1 AND 200),
  CHECK (length(request_fingerprint) = 64),
  CHECK (request_fingerprint = lower(request_fingerprint)),
  CHECK (request_fingerprint NOT GLOB '*[^0-9a-f]*'),
  CHECK (
    (status IN ('completed', 'cancelled') AND completed_at IS NOT NULL)
    OR
    (status IN ('queued', 'running', 'cancelling') AND completed_at IS NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_listing_verification_runs_owner
  ON listing_verification_runs (owner_user_id, id);

CREATE TABLE IF NOT EXISTS listing_verification_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL
    REFERENCES listing_verification_runs(id) ON DELETE CASCADE,
  listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  position INTEGER NOT NULL CHECK (position >= 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CONSTRAINT listing_verification_run_items_status_check CHECK (status IN (
      'queued', 'running', 'verified', 'pending_review',
      'blocked', 'failed', 'cancelled'
    )),
  attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  lease_token TEXT,
  lease_expires_at_epoch_seconds INTEGER,
  outcome_json TEXT,
  reason_code TEXT,
  reason TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  started_at TEXT,
  completed_at TEXT,
  UNIQUE (run_id, position),
  UNIQUE (run_id, listing_id),
  CHECK (lease_token IS NULL OR length(trim(lease_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running'
      AND lease_token IS NOT NULL
      AND lease_expires_at_epoch_seconds IS NOT NULL
      AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status <> 'running'
      AND lease_token IS NULL
      AND lease_expires_at_epoch_seconds IS NULL)
  ),
  CONSTRAINT listing_verification_run_items_completion_check CHECK (
    (status IN ('queued', 'running') AND completed_at IS NULL)
    OR
    (status IN (
      'verified', 'pending_review',
      'blocked', 'failed', 'cancelled'
    ) AND completed_at IS NOT NULL)
  ),
  CHECK (
    outcome_json IS NULL
    OR (
      length(outcome_json) BETWEEN 2 AND 65536
      AND json_valid(outcome_json)
      AND json_type(outcome_json) = 'object'
    )
  ),
  CONSTRAINT listing_verification_run_items_outcome_required_check CHECK (
    status NOT IN (
      'verified', 'pending_review', 'blocked'
    )
    OR outcome_json IS NOT NULL
  ),
  CHECK (reason_code IS NULL OR length(trim(reason_code)) BETWEEN 1 AND 100),
  CHECK (reason IS NULL OR length(trim(reason)) BETWEEN 1 AND 2000)
);

-- A listing may appear in run history many times, but may have only one
-- queued/running owner at a time across every process and run.
CREATE UNIQUE INDEX IF NOT EXISTS
  idx_listing_verification_run_items_one_active_listing
  ON listing_verification_run_items (listing_id)
  WHERE status IN ('queued', 'running');

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_listing_verification_run_items_one_running_per_run
  ON listing_verification_run_items (run_id)
  WHERE status = 'running';

CREATE INDEX IF NOT EXISTS idx_listing_verification_run_items_claim
  ON listing_verification_run_items (run_id, status, position, id);

-- Durable coordination for rebuilding listing state from trusted captures.
-- This operational ledger pins the exact extracted JSON needed for a durable
-- checkpoint; raw capture bytes, provider responses, and evidence remain in
-- their authoritative stores.
CREATE TABLE IF NOT EXISTS listing_replay_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manifest_version INTEGER NOT NULL CHECK (manifest_version > 0),
  manifest_sha256 TEXT NOT NULL UNIQUE,
  manifest_capture_count INTEGER NOT NULL CHECK (manifest_capture_count > 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'completed')),
  active_phase TEXT CHECK (active_phase IN ('extraction', 'materialization')),
  owner_token TEXT,
  heartbeat_at_epoch_seconds INTEGER,
  started_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(manifest_sha256) = 64),
  CHECK (manifest_sha256 = lower(manifest_sha256)),
  CHECK (manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (owner_token IS NULL OR length(trim(owner_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running' AND active_phase IS NOT NULL AND owner_token IS NOT NULL
      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status = 'queued' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)
    OR
    (status = 'completed' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)
  )
);

-- Only one batch owns replay mutations at a time. A stale owner is never
-- displaced implicitly; the command requires explicit conservative recovery.
CREATE UNIQUE INDEX IF NOT EXISTS idx_listing_replay_runs_one_running
  ON listing_replay_runs (status) WHERE status = 'running';

CREATE TABLE IF NOT EXISTS listing_replay_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES listing_replay_runs(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK (position >= 0),
  expected_rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT,
  extracted_listing_json TEXT,
  extraction_state TEXT NOT NULL DEFAULT 'queued'
    CHECK (extraction_state IN ('queued', 'running', 'succeeded', 'rejected', 'failed')),
  materialization_state TEXT NOT NULL DEFAULT 'blocked'
    CHECK (materialization_state IN ('blocked', 'queued', 'running', 'succeeded', 'rejected', 'failed')),
  resulting_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  terminal_rejection_phase TEXT
    CHECK (terminal_rejection_phase IN ('extraction', 'materialization')),
  terminal_rejection_stage TEXT CHECK (terminal_rejection_stage IN (
    'capture_admission', 'faa_aircraft_admission'
  )),
  terminal_rejection_reason_code TEXT CHECK (terminal_rejection_reason_code IN (
    'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed',
    'missing_registration', 'non_n_registration',
    'invalid_n_number', 'serial_conflict'
  )),
  last_failure_phase TEXT CHECK (last_failure_phase IN ('extraction', 'materialization')),
  last_failure_reason_code TEXT
    CHECK (last_failure_reason_code IN (
      'database_error', 'operation_failed', 'faa_lookup_failed', 'faa_listing_not_found',
      'faa_registry_snapshot_unavailable', 'faa_registration_not_found',
      'faa_registration_not_covered', 'faa_ambiguous_registration',
      'faa_registry_aircraft_identity_unavailable', 'faa_aircraft_manufacturer_mismatch',
      'faa_aircraft_model_mismatch', 'faa_canonical_identity_assignment_missing',
      'faa_canonical_identity_assignment_mismatch'
    )),
  extraction_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (extraction_attempt_count >= 0),
  materialization_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (materialization_attempt_count >= 0),
  extraction_started_at TEXT,
  extraction_completed_at TEXT,
  materialization_started_at TEXT,
  materialization_completed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (run_id, position),
  UNIQUE (run_id, plugin_submission_id),
  CHECK (length(expected_rendered_html_sha256) = 64),
  CHECK (expected_rendered_html_sha256 = lower(expected_rendered_html_sha256)),
  CHECK (expected_rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (extracted_listing_sha256 IS NULL OR (
    length(extracted_listing_sha256) = 64
    AND extracted_listing_sha256 = lower(extracted_listing_sha256)
    AND extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'
  )),
  CHECK (
    (extraction_state = 'rejected' AND materialization_state = 'blocked'
      AND terminal_rejection_phase = 'extraction'
      AND terminal_rejection_stage = 'capture_admission'
      AND terminal_rejection_reason_code IN (
        'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
      ))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'rejected'
      AND terminal_rejection_phase = 'materialization'
      AND (
        (terminal_rejection_stage = 'capture_admission'
          AND terminal_rejection_reason_code IN (
            'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
          ))
        OR
        (terminal_rejection_stage = 'faa_aircraft_admission'
          AND terminal_rejection_reason_code IN (
            'missing_registration', 'non_n_registration', 'invalid_n_number', 'serial_conflict'
          ))
      ))
    OR
    (extraction_state <> 'rejected' AND materialization_state <> 'rejected'
      AND terminal_rejection_phase IS NULL AND terminal_rejection_stage IS NULL
      AND terminal_rejection_reason_code IS NULL)
  ),
  CHECK (
    (extraction_state = 'failed' AND materialization_state = 'blocked'
      AND last_failure_phase = 'extraction'
      AND last_failure_reason_code IN ('database_error', 'operation_failed'))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'failed'
      AND last_failure_phase = 'materialization' AND last_failure_reason_code IS NOT NULL)
    OR
    (extraction_state <> 'failed' AND materialization_state <> 'failed'
      AND last_failure_phase IS NULL AND last_failure_reason_code IS NULL)
  ),
  CHECK ((materialization_state = 'succeeded') = (resulting_listing_id IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_sha256 IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_json IS NOT NULL)),
  CHECK (extraction_state = 'succeeded' OR materialization_state = 'blocked'),
  CHECK (extraction_state <> 'running' OR extraction_started_at IS NOT NULL),
  CHECK (materialization_state <> 'running' OR materialization_started_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_listing_replay_run_items_phase
  ON listing_replay_run_items (run_id, extraction_state, materialization_state, position);

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_checkpoint_exact_insert
BEFORE INSERT ON listing_replay_run_items
WHEN NEW.extraction_state = 'succeeded' AND NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  JOIN plugin_installs install ON install.id = submission.plugin_install_id
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.expected_rendered_html_sha256
    AND submission.extracted_listing_json IS NEW.extracted_listing_json
    AND submission.extraction_error IS NULL
    AND julianday(submission.submitted_at) IS NOT NULL
    AND (
      install.revoked_at IS NULL
      OR (
        julianday(install.revoked_at) IS NOT NULL
        AND julianday(submission.submitted_at) <= julianday(install.revoked_at)
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'replay extraction transition does not match its exact checkpoint');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_checkpoint_exact_update
BEFORE UPDATE ON listing_replay_run_items
WHEN NEW.extraction_state = 'succeeded' AND NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  JOIN plugin_installs install ON install.id = submission.plugin_install_id
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.expected_rendered_html_sha256
    AND submission.extracted_listing_json IS NEW.extracted_listing_json
    AND submission.extraction_error IS NULL
    AND julianday(submission.submitted_at) IS NOT NULL
    AND (
      install.revoked_at IS NULL
      OR (
        julianday(install.revoked_at) IS NOT NULL
        AND julianday(submission.submitted_at) <= julianday(install.revoked_at)
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'replay extraction transition does not match its exact checkpoint');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_completed_immutable_update
BEFORE UPDATE ON listing_replay_run_items
WHEN OLD.materialization_state = 'succeeded'
BEGIN
  SELECT RAISE(ABORT, 'completed replay item is immutable');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_completed_immutable_delete
BEFORE DELETE ON listing_replay_run_items
WHEN OLD.materialization_state = 'succeeded'
BEGIN
  SELECT RAISE(ABORT, 'completed replay item is immutable');
END;

CREATE TABLE IF NOT EXISTS plugin_submission_materialization_receipts (
  plugin_submission_id INTEGER PRIMARY KEY
    REFERENCES plugin_submissions(id) ON DELETE CASCADE,
  aircraft_sale_listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT NOT NULL,
  completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(rendered_html_sha256) = 64),
  CHECK (rendered_html_sha256 = lower(rendered_html_sha256)),
  CHECK (rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(extracted_listing_sha256) = 64),
  CHECK (extracted_listing_sha256 = lower(extracted_listing_sha256)),
  CHECK (extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE TRIGGER IF NOT EXISTS plugin_submission_materialization_receipts_immutable_update
BEFORE UPDATE ON plugin_submission_materialization_receipts
BEGIN
  SELECT RAISE(ABORT, 'replay materialization receipt is immutable');
END;

CREATE TRIGGER IF NOT EXISTS plugin_submission_materialization_receipts_immutable_delete
BEFORE DELETE ON plugin_submission_materialization_receipts
BEGIN
  SELECT RAISE(ABORT, 'replay materialization receipt is immutable');
END;

-- A replay checkpoint is a permanent binding to the exact authenticated
-- capture. The only permitted change after extraction is its one-time binding
-- to the unique same-owner source listing; a receipt closes that transition.
CREATE TRIGGER IF NOT EXISTS plugin_submissions_replay_checkpoint_immutable
BEFORE UPDATE ON plugin_submissions
WHEN (
  EXISTS (
    SELECT 1 FROM listing_replay_run_items item
    WHERE item.plugin_submission_id = OLD.id
      AND item.extraction_state = 'succeeded'
  )
  OR EXISTS (
    SELECT 1 FROM plugin_submission_materialization_receipts receipt
    WHERE receipt.plugin_submission_id = OLD.id
  )
) AND (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.plugin_install_id IS OLD.plugin_install_id)
  OR NOT (NEW.source_url IS OLD.source_url)
  OR NOT (NEW.submitted_at IS OLD.submitted_at)
  OR NOT (NEW.rendered_html IS OLD.rendered_html)
  OR NOT (NEW.rendered_html_sha256 IS OLD.rendered_html_sha256)
  OR NOT (NEW.signature_base64 IS OLD.signature_base64)
  OR NOT (NEW.extracted_listing_json IS OLD.extracted_listing_json)
  OR NOT (NEW.extraction_error IS OLD.extraction_error)
  OR NOT (
    NEW.canonical_listing_id IS OLD.canonical_listing_id
    OR (
      OLD.canonical_listing_id IS NULL
      AND NEW.canonical_listing_id IS NOT NULL
      AND NOT EXISTS (
        SELECT 1 FROM plugin_submission_materialization_receipts receipt
        WHERE receipt.plugin_submission_id = OLD.id
      )
      AND EXISTS (
        SELECT 1 FROM aircraft_sale_listings listing
        WHERE listing.id = NEW.canonical_listing_id
          AND listing.created_by_user_id = OLD.user_id
          AND listing.source_url = OLD.source_url
      )
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'replay checkpoint capture is immutable');
END;

-- Public-key identity is immutable once used by a replay checkpoint.
-- Revocation remains a legitimate monotonic lifecycle transition, provided
-- every pinned capture predates the parsed revocation instant.
CREATE TRIGGER IF NOT EXISTS plugin_installs_replay_identity_immutable
BEFORE UPDATE ON plugin_installs
WHEN EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  WHERE submission.plugin_install_id = OLD.id
    AND (
      EXISTS (
        SELECT 1 FROM listing_replay_run_items item
        WHERE item.plugin_submission_id = submission.id
          AND item.extraction_state = 'succeeded'
      )
      OR EXISTS (
        SELECT 1 FROM plugin_submission_materialization_receipts receipt
        WHERE receipt.plugin_submission_id = submission.id
      )
    )
) AND (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.public_key_base64 IS OLD.public_key_base64)
  OR NOT (NEW.created_at IS OLD.created_at)
  OR NOT (
    NEW.revoked_at IS OLD.revoked_at
    OR (
      OLD.revoked_at IS NULL
      AND NEW.revoked_at IS NOT NULL
      AND julianday(NEW.revoked_at) IS NOT NULL
      AND NOT EXISTS (
        SELECT 1
        FROM plugin_submissions submission
        WHERE submission.plugin_install_id = OLD.id
          AND (
            EXISTS (
              SELECT 1 FROM listing_replay_run_items item
              WHERE item.plugin_submission_id = submission.id
                AND item.extraction_state = 'succeeded'
            )
            OR EXISTS (
              SELECT 1 FROM plugin_submission_materialization_receipts receipt
              WHERE receipt.plugin_submission_id = submission.id
            )
          )
          AND (
            julianday(submission.submitted_at) IS NULL
            OR julianday(submission.submitted_at) > julianday(NEW.revoked_at)
          )
      )
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'replay checkpoint plugin identity is immutable');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_listing_replay_runs', 1,
  'ef344cdb9cf9a7ffcd0ae66e1c9cb3979afa07c1155377cee5dc1031dd0d47c1',
  CURRENT_TIMESTAMP
) ON CONFLICT (migration_name) DO NOTHING;

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  avionics_model_id INTEGER NOT NULL REFERENCES avionics_models(id),
  quantity INTEGER NOT NULL DEFAULT 1,
  source TEXT NOT NULL DEFAULT 'listing',
  source_notes TEXT,
  configuration_action TEXT NOT NULL DEFAULT 'installed'
    CHECK (configuration_action IN ('installed', 'replaces', 'removes')),
  replaces_avionics_model_id INTEGER REFERENCES avionics_models(id),
  source_confidence TEXT
    CHECK (source_confidence IS NULL OR source_confidence IN ('high', 'medium', 'low')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_sale_listing_id, avionics_model_id),
  CHECK (
    (configuration_action = 'installed' AND replaces_avionics_model_id IS NULL)
    OR
    (configuration_action IN ('replaces', 'removes') AND replaces_avionics_model_id IS NOT NULL)
  )
);

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_approved_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
OR (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models replaced_model
    WHERE replaced_model.id = NEW.replaces_avionics_model_id
      AND replaced_model.catalog_status = 'approved'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics association requires approved catalog entries');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_approved_update
BEFORE UPDATE OF avionics_model_id, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
WHEN (
  NEW.avionics_model_id IS NOT OLD.avionics_model_id
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models model
    WHERE model.id = NEW.avionics_model_id
      AND model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = OLD.avionics_model_id
    JOIN aircraft_sale_listings listing
      ON listing.id = NEW.aircraft_sale_listing_id
    WHERE guard.duplicate_model_id = OLD.avionics_model_id
      AND guard.survivor_model_id = NEW.avionics_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
      AND listing.ingestion_state <> 'ready'
      AND listing.is_verified = 0
  )
)
OR (
  NEW.replaces_avionics_model_id IS NOT OLD.replaces_avionics_model_id
  AND
  NEW.replaces_avionics_model_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_models replaced_model
    WHERE replaced_model.id = NEW.replaces_avionics_model_id
      AND replaced_model.catalog_status = 'approved'
  )
  AND NOT EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = OLD.replaces_avionics_model_id
    JOIN aircraft_sale_listings listing
      ON listing.id = NEW.aircraft_sale_listing_id
    WHERE guard.duplicate_model_id = OLD.replaces_avionics_model_id
      AND guard.survivor_model_id = NEW.replaces_avionics_model_id
      AND survivor.catalog_status = 'unreviewed'
      AND legacy.catalog_status = 'unreviewed'
      AND listing.ingestion_state <> 'ready'
      AND listing.is_verified = 0
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics association requires approved catalog entries');
END;

CREATE UNIQUE INDEX IF NOT EXISTS
  idx_aircraft_sale_listing_avionics_unique_displacement
  ON aircraft_sale_listing_avionics (
    aircraft_sale_listing_id, replaces_avionics_model_id
  )
  WHERE configuration_action IN ('replaces', 'removes');

-- Audit surfaces for legacy action graphs. Non-ready legacy listings may
-- retain invalid rows until review; no represented listing may become ready.
CREATE VIEW IF NOT EXISTS avionics_semantic_duplicate_listing_links AS
SELECT
  link.aircraft_sale_listing_id AS listing_id,
  identity.avionics_manufacturer_identity_id,
  identity.canonical_product_key,
  COUNT(*) AS link_count
FROM aircraft_sale_listing_avionics link
JOIN avionics_approved_product_graph_identities identity
  ON identity.avionics_model_id = link.avionics_model_id
WHERE link.configuration_action IN ('installed', 'replaces')
GROUP BY
  link.aircraft_sale_listing_id,
  identity.avionics_manufacturer_identity_id,
  identity.canonical_product_key
HAVING COUNT(*) > 1;

CREATE VIEW IF NOT EXISTS avionics_semantic_invalid_replacement_links AS
SELECT link.id AS listing_link_id, link.aircraft_sale_listing_id AS listing_id
FROM aircraft_sale_listing_avionics link
LEFT JOIN avionics_approved_product_graph_identities subject
  ON subject.avionics_model_id = link.avionics_model_id
LEFT JOIN avionics_approved_product_graph_identities displaced
  ON displaced.avionics_model_id = link.replaces_avionics_model_id
WHERE (link.configuration_action = 'installed'
    AND link.replaces_avionics_model_id IS NOT NULL)
  OR (link.configuration_action = 'replaces' AND (
    link.replaces_avionics_model_id IS NULL
    OR link.replaces_avionics_model_id = link.avionics_model_id
    OR (
      subject.avionics_manufacturer_identity_id
        = displaced.avionics_manufacturer_identity_id
      AND subject.canonical_product_key = displaced.canonical_product_key
    )
  ))
  OR (link.configuration_action = 'removes'
    AND link.replaces_avionics_model_id IS NOT link.avionics_model_id);

CREATE VIEW IF NOT EXISTS avionics_semantic_duplicate_displacement_targets AS
SELECT
  link.aircraft_sale_listing_id AS listing_id,
  displaced.avionics_manufacturer_identity_id,
  displaced.canonical_product_key,
  COUNT(*) AS link_count
FROM aircraft_sale_listing_avionics link
JOIN avionics_approved_product_graph_identities displaced
  ON displaced.avionics_model_id = link.replaces_avionics_model_id
WHERE link.configuration_action IN ('replaces', 'removes')
GROUP BY
  link.aircraft_sale_listing_id,
  displaced.avionics_manufacturer_identity_id,
  displaced.canonical_product_key
HAVING COUNT(*) > 1;

CREATE VIEW IF NOT EXISTS avionics_semantic_installed_displacement_conflicts AS
SELECT DISTINCT
  installed.aircraft_sale_listing_id AS listing_id,
  subject.avionics_manufacturer_identity_id,
  subject.canonical_product_key
FROM aircraft_sale_listing_avionics installed
JOIN avionics_approved_product_graph_identities subject
  ON subject.avionics_model_id = installed.avionics_model_id
JOIN aircraft_sale_listing_avionics displacement
  ON displacement.aircraft_sale_listing_id
    = installed.aircraft_sale_listing_id
 AND displacement.configuration_action IN ('replaces', 'removes')
JOIN avionics_approved_product_graph_identities displaced
  ON displaced.avionics_model_id
    = displacement.replaces_avionics_model_id
 AND displaced.avionics_manufacturer_identity_id
    = subject.avionics_manufacturer_identity_id
 AND displaced.canonical_product_key = subject.canonical_product_key
WHERE installed.configuration_action IN ('installed', 'replaces');

CREATE VIEW IF NOT EXISTS avionics_semantic_invalid_listing_action_graphs AS
SELECT listing_id, 'duplicate_installed_subject' AS issue
FROM avionics_semantic_duplicate_listing_links
UNION
SELECT listing_id, 'invalid_action_subject_target' AS issue
FROM avionics_semantic_invalid_replacement_links
UNION
SELECT listing_id, 'duplicate_displacement_target' AS issue
FROM avionics_semantic_duplicate_displacement_targets
UNION
SELECT listing_id, 'installed_subject_is_displaced' AS issue
FROM avionics_semantic_installed_displacement_conflicts;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_mutable_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND (listing.ingestion_state = 'ready' OR listing.is_verified = 1)
)
BEGIN
  SELECT RAISE(ABORT, 'ready or verified listing avionics are immutable');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_mutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id IN (
      OLD.aircraft_sale_listing_id, NEW.aircraft_sale_listing_id
    )
    AND (listing.ingestion_state = 'ready' OR listing.is_verified = 1)
)
BEGIN
  SELECT RAISE(ABORT, 'ready or verified listing avionics are immutable');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_mutable_delete
BEFORE DELETE ON aircraft_sale_listing_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = OLD.aircraft_sale_listing_id
    AND (listing.ingestion_state = 'ready' OR listing.is_verified = 1)
)
BEGIN
  SELECT RAISE(ABORT, 'ready or verified listing avionics are immutable');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_distinct_replacement_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN (NEW.configuration_action = 'installed'
    AND NEW.replaces_avionics_model_id IS NOT NULL)
  OR (NEW.configuration_action = 'replaces' AND (
    NEW.replaces_avionics_model_id IS NULL
    OR NEW.replaces_avionics_model_id = NEW.avionics_model_id
    OR EXISTS (
      SELECT 1
      FROM avionics_approved_product_graph_identities subject
      JOIN avionics_approved_product_graph_identities displaced
        ON displaced.avionics_model_id = NEW.replaces_avionics_model_id
       AND displaced.avionics_manufacturer_identity_id
         = subject.avionics_manufacturer_identity_id
       AND displaced.canonical_product_key = subject.canonical_product_key
      WHERE subject.avionics_model_id = NEW.avionics_model_id
    )
  ))
  OR (NEW.configuration_action = 'removes'
    AND NEW.replaces_avionics_model_id IS NOT NEW.avionics_model_id)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action has invalid subject/target semantics');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_distinct_replacement_update
BEFORE UPDATE OF avionics_model_id, configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
WHEN (NEW.configuration_action = 'installed'
    AND NEW.replaces_avionics_model_id IS NOT NULL)
  OR (NEW.configuration_action = 'replaces' AND (
    NEW.replaces_avionics_model_id IS NULL
    OR NEW.replaces_avionics_model_id = NEW.avionics_model_id
    OR EXISTS (
      SELECT 1
      FROM avionics_approved_product_graph_identities subject
      JOIN avionics_approved_product_graph_identities displaced
        ON displaced.avionics_model_id = NEW.replaces_avionics_model_id
       AND displaced.avionics_manufacturer_identity_id
         = subject.avionics_manufacturer_identity_id
       AND displaced.canonical_product_key = subject.canonical_product_key
      WHERE subject.avionics_model_id = NEW.avionics_model_id
    )
  ))
  OR (NEW.configuration_action = 'removes'
    AND NEW.replaces_avionics_model_id IS NOT NEW.avionics_model_id)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action has invalid subject/target semantics');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_semantic_unique_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN NEW.configuration_action IN ('installed', 'replaces')
AND EXISTS (
  SELECT 1
  FROM avionics_approved_product_graph_identities candidate
  JOIN aircraft_sale_listing_avionics existing
    ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
   AND existing.configuration_action IN ('installed', 'replaces')
  JOIN avionics_approved_product_graph_identities existing_identity
    ON existing_identity.avionics_model_id = existing.avionics_model_id
   AND existing_identity.avionics_manufacturer_identity_id
     = candidate.avionics_manufacturer_identity_id
   AND existing_identity.canonical_product_key = candidate.canonical_product_key
  WHERE candidate.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'listing cannot install one canonical avionics product more than once');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_semantic_unique_update
BEFORE UPDATE OF aircraft_sale_listing_id, avionics_model_id, configuration_action
ON aircraft_sale_listing_avionics
WHEN NEW.configuration_action IN ('installed', 'replaces')
AND EXISTS (
  SELECT 1
  FROM avionics_approved_product_graph_identities candidate
  JOIN aircraft_sale_listing_avionics existing
    ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
   AND existing.id <> OLD.id
   AND existing.configuration_action IN ('installed', 'replaces')
  JOIN avionics_approved_product_graph_identities existing_identity
    ON existing_identity.avionics_model_id = existing.avionics_model_id
   AND existing_identity.avionics_manufacturer_identity_id
     = candidate.avionics_manufacturer_identity_id
   AND existing_identity.canonical_product_key = candidate.canonical_product_key
  WHERE candidate.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'listing cannot install one canonical avionics product more than once');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_action_graph_insert
BEFORE INSERT ON aircraft_sale_listing_avionics
WHEN (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
OR (
  NEW.configuration_action IN ('installed', 'replaces')
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.avionics_model_id
  )
)
OR (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.configuration_action IN ('installed', 'replaces')
    JOIN avionics_approved_product_graph_identities existing_subject
      ON existing_subject.avionics_model_id = existing.avionics_model_id
     AND existing_subject.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_subject.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action graph has duplicate or contradictory installed/displaced identities');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listing_avionics_action_graph_update
BEFORE UPDATE OF aircraft_sale_listing_id, avionics_model_id,
  configuration_action, replaces_avionics_model_id
ON aircraft_sale_listing_avionics
WHEN (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.id <> OLD.id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
OR (
  NEW.configuration_action IN ('installed', 'replaces')
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.id <> OLD.id
     AND existing.configuration_action IN ('replaces', 'removes')
    JOIN avionics_approved_product_graph_identities existing_target
      ON existing_target.avionics_model_id
        = existing.replaces_avionics_model_id
     AND existing_target.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_target.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.avionics_model_id
  )
)
OR (
  NEW.replaces_avionics_model_id IS NOT NULL
  AND EXISTS (
    SELECT 1
    FROM avionics_approved_product_graph_identities candidate
    JOIN aircraft_sale_listing_avionics existing
      ON existing.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
     AND existing.id <> OLD.id
     AND existing.configuration_action IN ('installed', 'replaces')
    JOIN avionics_approved_product_graph_identities existing_subject
      ON existing_subject.avionics_model_id = existing.avionics_model_id
     AND existing_subject.avionics_manufacturer_identity_id
       = candidate.avionics_manufacturer_identity_id
     AND existing_subject.canonical_product_key = candidate.canonical_product_key
    WHERE candidate.avionics_model_id = NEW.replaces_avionics_model_id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'listing avionics action graph has duplicate or contradictory installed/displaced identities');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listings_ready_semantic_avionics
BEFORE UPDATE OF ingestion_state ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
AND (
  EXISTS (
    SELECT 1
    FROM avionics_semantic_invalid_listing_action_graphs invalid_graph
    WHERE invalid_graph.listing_id = NEW.id
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics link
    JOIN avionics_models model ON model.id = link.avionics_model_id
    WHERE link.aircraft_sale_listing_id = NEW.id
      AND model.catalog_status <> 'approved'
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics link
    JOIN avionics_models model ON model.id = link.replaces_avionics_model_id
    WHERE link.aircraft_sale_listing_id = NEW.id
      AND model.catalog_status <> 'approved'
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics link
    WHERE link.aircraft_sale_listing_id = NEW.id
      AND (
        link.quantity <= 0
        OR link.source_confidence IS NOT 'high'
        OR link.source NOT IN ('listing', 'listing_review')
      )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'ready listing requires unique approved canonical avionics');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_sale_listings_ready_semantic_avionics_insert
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
BEGIN
  SELECT RAISE(ABORT, 'listing cannot be inserted ready before avionics are validated');
END;

CREATE TRIGGER IF NOT EXISTS listing_verified_requires_ready_insert
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.is_verified = 1 AND NEW.ingestion_state <> 'ready'
BEGIN
  SELECT RAISE(ABORT, 'verified listing must be in the ready ingestion state');
END;

CREATE TRIGGER IF NOT EXISTS listing_verified_requires_ready_update
BEFORE UPDATE OF is_verified, ingestion_state ON aircraft_sale_listings
WHEN NEW.is_verified = 1 AND NEW.ingestion_state <> 'ready'
BEGIN
  SELECT RAISE(ABORT, 'verified listing must be in the ready ingestion state');
END;

CREATE TRIGGER IF NOT EXISTS avionics_models_referenced_status_update
BEFORE UPDATE OF catalog_status ON avionics_models
WHEN NEW.catalog_status <> 'approved'
AND (
  EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_avionics listing_link
    WHERE listing_link.avionics_model_id = OLD.id
       OR listing_link.replaces_avionics_model_id = OLD.id
  )
  OR EXISTS (
    SELECT 1
    FROM avionics_suite_components suite_link
    WHERE suite_link.suite_model_id = OLD.id
       OR suite_link.component_model_id = OLD.id
  )
  OR EXISTS (
    SELECT 1
    FROM aircraft_reference_avionics reference_link
    WHERE reference_link.avionics_model_id = OLD.id
  )
)
BEGIN
  SELECT RAISE(ABORT, 'referenced avionics catalog entry cannot be unapproved');
END;

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_facts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  fact_kind TEXT NOT NULL CHECK (fact_kind IN (
    'restoration', 'damage_history', 'log_completeness', 'paint_condition',
    'interior_condition', 'engine_conversion', 'airframe_conversion', 'major_modification'
  )),
  fact_value TEXT NOT NULL,
  evidence_text TEXT NOT NULL,
  source_url TEXT,
  source_confidence TEXT NOT NULL CHECK (source_confidence IN ('high', 'medium', 'low')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_sale_listing_id, fact_kind, fact_value, evidence_text)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_sale_listing_facts_listing
  ON aircraft_sale_listing_facts (aircraft_sale_listing_id, fact_kind);

CREATE TABLE IF NOT EXISTS valuation_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  capture_time TEXT NOT NULL,
  input_sha256 TEXT NOT NULL UNIQUE,
  selection_policy_json TEXT NOT NULL,
  feature_schema_version INTEGER NOT NULL,
  included_count INTEGER NOT NULL,
  excluded_count INTEGER NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS valuation_snapshot_rows (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id INTEGER NOT NULL REFERENCES valuation_snapshots(id) ON DELETE CASCADE,
  source_listing_id INTEGER NOT NULL,
  duplicate_group_key TEXT NOT NULL,
  inclusion_flag INTEGER NOT NULL CHECK (inclusion_flag IN (0, 1)),
  exclusion_reason TEXT,
  feature_json TEXT NOT NULL,
  target_price_usd REAL,
  row_sha256 TEXT NOT NULL,
  UNIQUE (snapshot_id, source_listing_id)
);

CREATE INDEX IF NOT EXISTS idx_valuation_snapshot_rows_snapshot_inclusion
  ON valuation_snapshot_rows (snapshot_id, inclusion_flag, source_listing_id);

CREATE TABLE IF NOT EXISTS valuation_model_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  snapshot_id INTEGER NOT NULL REFERENCES valuation_snapshots(id),
  model_kind TEXT NOT NULL CHECK (model_kind IN ('structural', 'dnn')),
  artifact_format_version INTEGER NOT NULL,
  state TEXT NOT NULL CHECK (state IN ('candidate', 'active', 'retired')),
  metrics_json TEXT NOT NULL,
  configuration_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_valuation_model_versions_one_active_kind
  ON valuation_model_versions (model_kind) WHERE state = 'active';

CREATE TABLE IF NOT EXISTS valuation_model_artifacts (
  model_version_id INTEGER NOT NULL
    REFERENCES valuation_model_versions(id) ON DELETE CASCADE,
  artifact_name TEXT NOT NULL,
  artifact_bytes BLOB NOT NULL,
  sha256 TEXT NOT NULL,
  media_type TEXT NOT NULL,
  PRIMARY KEY (model_version_id, artifact_name)
);

CREATE TABLE IF NOT EXISTS valuation_fold_predictions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  model_version_id INTEGER NOT NULL
    REFERENCES valuation_model_versions(id) ON DELETE CASCADE,
  fold_id TEXT NOT NULL,
  duplicate_group_key TEXT NOT NULL,
  source_listing_id INTEGER NOT NULL,
  actual_price_usd REAL NOT NULL,
  predicted_price_usd REAL NOT NULL,
  log_error REAL NOT NULL,
  absolute_percentage_error REAL NOT NULL,
  support_grade TEXT NOT NULL CHECK (support_grade IN ('low', 'medium', 'high'))
);

CREATE INDEX IF NOT EXISTS idx_valuation_fold_predictions_model
  ON valuation_fold_predictions (model_version_id, fold_id);

CREATE TABLE IF NOT EXISTS valuation_refresh_state (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  listings_changed_at TEXT NOT NULL,
  reason TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rental_clubs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  location TEXT NOT NULL,
  airport_code TEXT,
  website_url TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (normalized_name, location)
);

CREATE TABLE IF NOT EXISTS rental_club_cost_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  rental_club_id INTEGER NOT NULL REFERENCES rental_clubs(id),
  effective_from TEXT NOT NULL,
  effective_to TEXT,
  insurance_annual_usd REAL NOT NULL DEFAULT 0,
  club_monthly_usd REAL NOT NULL DEFAULT 0,
  club_annual_usd REAL NOT NULL DEFAULT 0,
  initiation_fee_usd REAL NOT NULL DEFAULT 0,
  source_url TEXT,
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (effective_to IS NULL OR effective_to > effective_from)
);

CREATE TABLE IF NOT EXISTS rental_aircraft_offerings (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  rental_club_id INTEGER NOT NULL REFERENCES rental_clubs(id),
  aircraft_model_variant_id INTEGER NOT NULL REFERENCES aircraft_model_variants(id),
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  display_name TEXT NOT NULL,
  tail_number TEXT,
  is_active INTEGER NOT NULL DEFAULT 1 CHECK (is_active IN (0, 1)),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS rental_rate_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  rental_aircraft_offering_id INTEGER NOT NULL REFERENCES rental_aircraft_offerings(id),
  effective_from TEXT NOT NULL,
  effective_to TEXT,
  rental_rate_per_hour_usd REAL NOT NULL,
  rate_type TEXT NOT NULL DEFAULT 'wet',
  billing_meter TEXT NOT NULL DEFAULT 'hobbs',
  source_url TEXT,
  created_by_user_id INTEGER NOT NULL REFERENCES users(id),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (effective_to IS NULL OR effective_to > effective_from)
);


-- Curated aircraft identity and immutable reference configurations.
CREATE TABLE IF NOT EXISTS curation_evidence_sources (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_url TEXT NOT NULL,
  resolved_url TEXT,
  source_title TEXT NOT NULL,
  publisher TEXT,
  source_domain TEXT NOT NULL,
  source_tier TEXT NOT NULL CHECK (source_tier IN (
    'manufacturer_primary', 'regulator_primary', 'recognized_secondary',
    'marketplace_observation'
  )),
  content_sha256 TEXT,
  retrieved_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source_url, content_sha256)
);

CREATE INDEX IF NOT EXISTS idx_curation_evidence_sources_domain
  ON curation_evidence_sources (source_domain, source_tier);

CREATE TABLE IF NOT EXISTS curation_evidence_claims (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence_source_id INTEGER NOT NULL
    REFERENCES curation_evidence_sources(id) ON DELETE RESTRICT,
  claim_kind TEXT NOT NULL CHECK (claim_kind IN (
    'identity', 'alias', 'applicability', 'standard_equipment', 'price',
    'specification', 'package_composition', 'other'
  )),
  subject_text TEXT NOT NULL,
  predicate_text TEXT NOT NULL,
  object_text TEXT NOT NULL,
  quoted_evidence TEXT NOT NULL,
  citation_start INTEGER,
  citation_end INTEGER,
  validation_status TEXT NOT NULL DEFAULT 'captured'
    CHECK (validation_status IN ('captured', 'validated', 'rejected')),
  validated_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(quoted_evidence)) > 0),
  CHECK (
    (citation_start IS NULL AND citation_end IS NULL)
    OR (
      citation_start IS NOT NULL AND citation_start >= 0
      AND citation_end IS NOT NULL AND citation_end > citation_start
    )
  ),
  CHECK (
    validation_status <> 'validated'
    OR validated_at IS NOT NULL
  )
);

CREATE INDEX IF NOT EXISTS idx_curation_evidence_claims_source
  ON curation_evidence_claims (evidence_source_id, claim_kind);

CREATE TRIGGER IF NOT EXISTS curation_evidence_sources_immutable_update
BEFORE UPDATE ON curation_evidence_sources
BEGIN SELECT RAISE(ABORT, 'curation evidence sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS curation_evidence_sources_immutable_delete
BEFORE DELETE ON curation_evidence_sources
BEGIN SELECT RAISE(ABORT, 'curation evidence sources are immutable'); END;
CREATE TRIGGER IF NOT EXISTS curation_evidence_claims_validate_once
BEFORE UPDATE ON curation_evidence_claims
WHEN OLD.validation_status <> 'captured'
  OR NEW.validation_status NOT IN ('validated', 'rejected')
  OR NEW.evidence_source_id <> OLD.evidence_source_id
  OR NEW.claim_kind <> OLD.claim_kind
  OR NEW.subject_text <> OLD.subject_text
  OR NEW.predicate_text <> OLD.predicate_text
  OR NEW.object_text <> OLD.object_text
  OR NEW.quoted_evidence <> OLD.quoted_evidence
  OR NEW.citation_start IS NOT OLD.citation_start
  OR NEW.citation_end IS NOT OLD.citation_end
BEGIN SELECT RAISE(ABORT, 'curation evidence claims are append-only and validate once'); END;
CREATE TRIGGER IF NOT EXISTS curation_evidence_claims_immutable_delete
BEFORE DELETE ON curation_evidence_claims
BEGIN SELECT RAISE(ABORT, 'curation evidence claims are immutable'); END;

-- Provider telemetry is separate from domain decisions and evidence. One row
-- represents one logical Gemini request, including any transport retries.
CREATE TABLE IF NOT EXISTS gemini_api_usage (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  task TEXT NOT NULL,
  purpose TEXT NOT NULL,
  api_family TEXT NOT NULL
    CHECK (api_family IN ('generate_content', 'interactions')),
  api_version TEXT,
  model TEXT NOT NULL,
  service_tier TEXT NOT NULL DEFAULT 'standard',
  status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN (
    'pending', 'completed', 'failed', 'cancelled', 'incomplete',
    'requires_action', 'budget_exceeded'
  )),
  validation_status TEXT NOT NULL DEFAULT 'not_evaluated'
    CHECK (validation_status IN ('not_evaluated', 'accepted', 'rejected')),
  provider_request_id TEXT,
  correlation_id TEXT,
  request_fingerprint TEXT,
  aircraft_sale_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  source_kind TEXT,
  source_id TEXT,
  input_tokens INTEGER CHECK (input_tokens IS NULL OR input_tokens >= 0),
  output_tokens INTEGER CHECK (output_tokens IS NULL OR output_tokens >= 0),
  thought_tokens INTEGER CHECK (thought_tokens IS NULL OR thought_tokens >= 0),
  cached_tokens INTEGER CHECK (cached_tokens IS NULL OR cached_tokens >= 0),
  tool_tokens INTEGER CHECK (tool_tokens IS NULL OR tool_tokens >= 0),
  search_query_count INTEGER
    CHECK (search_query_count IS NULL OR search_query_count >= 0),
  attempt_count INTEGER NOT NULL DEFAULT 1 CHECK (attempt_count >= 1),
  retry_count INTEGER NOT NULL DEFAULT 0
    CHECK (retry_count >= 0 AND retry_count = attempt_count - 1),
  latency_ms INTEGER CHECK (latency_ms IS NULL OR latency_ms >= 0),
  error_text TEXT,
  estimated_cost_microusd INTEGER
    CHECK (estimated_cost_microusd IS NULL OR estimated_cost_microusd >= 0),
  pricing_snapshot_json TEXT,
  started_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(trim(task)) > 0),
  CHECK (length(trim(purpose)) > 0),
  CHECK (api_version IS NULL OR length(trim(api_version)) > 0),
  CHECK (length(trim(model)) > 0),
  CHECK (length(trim(service_tier)) > 0),
  CHECK (provider_request_id IS NULL OR length(trim(provider_request_id)) > 0),
  CHECK (correlation_id IS NULL OR length(trim(correlation_id)) > 0),
  CHECK (request_fingerprint IS NULL OR length(trim(request_fingerprint)) > 0),
  CHECK (
    (source_kind IS NULL AND source_id IS NULL)
    OR (
      source_kind IS NOT NULL AND length(trim(source_kind)) > 0
      AND source_id IS NOT NULL AND length(trim(source_id)) > 0
    )
  ),
  CHECK (
    (estimated_cost_microusd IS NULL AND pricing_snapshot_json IS NULL)
    OR (estimated_cost_microusd IS NOT NULL AND pricing_snapshot_json IS NOT NULL)
  ),
  CHECK (
    (status = 'pending' AND completed_at IS NULL)
    OR (status <> 'pending' AND completed_at IS NOT NULL)
  ),
  CHECK (status = 'completed' OR validation_status = 'not_evaluated'),
  CHECK (status <> 'failed' OR length(trim(error_text)) > 0),
  CHECK (status <> 'completed' OR error_text IS NULL)
);

CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_correlation
  ON gemini_api_usage (correlation_id, id);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_task_model
  ON gemini_api_usage (task, purpose, model, service_tier, started_at);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_listing
  ON gemini_api_usage (aircraft_sale_listing_id, started_at);
CREATE INDEX IF NOT EXISTS idx_gemini_api_usage_source
  ON gemini_api_usage (source_kind, source_id, started_at);

CREATE TABLE IF NOT EXISTS aircraft_identity_observations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  source_url TEXT,
  observed_make TEXT,
  observed_family TEXT,
  observed_designation TEXT,
  observed_generation TEXT,
  observed_package TEXT,
  model_year INTEGER CHECK (model_year IS NULL OR model_year BETWEEN 1900 AND 2200),
  serial_number TEXT,
  registration_number TEXT,
  market_code TEXT,
  exact_source_evidence TEXT NOT NULL,
  observation_sha256 TEXT NOT NULL UNIQUE,
  legacy_hint_json TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(exact_source_evidence)) > 0)
);

CREATE INDEX IF NOT EXISTS idx_aircraft_identity_observations_listing
  ON aircraft_identity_observations (aircraft_sale_listing_id);

CREATE TABLE IF NOT EXISTS aircraft_identity_resolution_cases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  observation_id INTEGER NOT NULL
    REFERENCES aircraft_identity_observations(id) ON DELETE CASCADE,
  resolution_scope TEXT NOT NULL CHECK (resolution_scope IN (
    'make', 'family', 'designation', 'generation', 'package',
    'engine_model', 'propeller_model',
    'reference_configuration', 'reference_profile'
  )),
  job_fingerprint TEXT NOT NULL UNIQUE,
  catalog_revision TEXT NOT NULL,
  case_status TEXT NOT NULL DEFAULT 'open'
    CHECK (case_status IN ('open', 'adjudicating', 'resolved', 'blocked')),
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_aircraft_identity_cases_observation
  ON aircraft_identity_resolution_cases (observation_id, resolution_scope);

CREATE TABLE IF NOT EXISTS aircraft_identity_resolution_candidates (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE CASCADE,
  candidate_kind TEXT NOT NULL CHECK (candidate_kind IN (
    'make', 'family', 'designation', 'generation', 'package',
    'engine_model', 'propeller_model',
    'reference_configuration', 'new_entity'
  )),
  candidate_entity_id INTEGER,
  rank INTEGER NOT NULL CHECK (rank >= 1),
  retrieval_method TEXT NOT NULL,
  retrieval_score REAL,
  candidate_snapshot_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (resolution_case_id, candidate_kind, rank),
  CHECK (
    (candidate_kind = 'new_entity' AND candidate_entity_id IS NULL)
    OR (candidate_kind <> 'new_entity' AND candidate_entity_id IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS aircraft_identity_decisions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE RESTRICT,
  entity_kind TEXT NOT NULL CHECK (entity_kind IN (
    'make', 'family', 'designation', 'alias', 'identifier', 'generation',
    'generation_designation', 'package', 'package_applicability',
    'engine_model', 'propeller_model',
    'reference_configuration', 'serial_scheme', 'feature_definition',
    'reference_profile'
  )),
  decision_action TEXT NOT NULL CHECK (decision_action IN (
    'match_existing', 'approve_new', 'no_supported_selection', 'ambiguous',
    'reject'
  )),
  decision_status TEXT NOT NULL CHECK (decision_status IN (
    'approved', 'rejected', 'ambiguous'
  )),
  selected_entity_id INTEGER,
  decision_payload_json TEXT NOT NULL,
  deterministic_validation_json TEXT NOT NULL,
  deterministic_validation_passed INTEGER NOT NULL
    CHECK (deterministic_validation_passed IN (0, 1)),
  rationale TEXT NOT NULL,
  decided_by_user_id INTEGER REFERENCES users(id) ON DELETE RESTRICT,
  decided_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (decision_status = 'approved'
      AND decision_action IN (
        'match_existing', 'approve_new', 'no_supported_selection'
      )
      AND deterministic_validation_passed = 1)
    OR (decision_status = 'rejected'
      AND decision_action = 'reject')
    OR (decision_status = 'ambiguous' AND decision_action = 'ambiguous')
  ),
  CHECK (
    (decision_action = 'match_existing' AND selected_entity_id IS NOT NULL)
    OR (decision_action <> 'match_existing' AND selected_entity_id IS NULL)
  ),
  CHECK (
    decision_action <> 'no_supported_selection'
    OR entity_kind IN ('generation', 'package')
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_identity_decisions_case
  ON aircraft_identity_decisions (resolution_case_id, decision_status);

CREATE TABLE IF NOT EXISTS aircraft_identity_decision_claims (
  decision_id INTEGER NOT NULL
    REFERENCES aircraft_identity_decisions(id) ON DELETE CASCADE,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  evidence_role TEXT NOT NULL CHECK (evidence_role IN (
    'identity', 'difference', 'applicability', 'standard_equipment',
    'price', 'specification'
  )),
  PRIMARY KEY (decision_id, evidence_claim_id, evidence_role)
);

CREATE TRIGGER IF NOT EXISTS aircraft_identity_no_supported_selection_claim_insert
BEFORE INSERT ON aircraft_identity_decision_claims
WHEN EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  WHERE decision.id = NEW.decision_id
    AND decision.decision_action = 'no_supported_selection'
)
BEGIN
  SELECT RAISE(ABORT, 'no-supported-selection decision cannot have evidence claims');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_identity_no_supported_selection_claim_update
BEFORE UPDATE OF decision_id ON aircraft_identity_decision_claims
WHEN EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  WHERE decision.id = NEW.decision_id
    AND decision.decision_action = 'no_supported_selection'
)
BEGIN
  SELECT RAISE(ABORT, 'no-supported-selection decision cannot have evidence claims');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_identity_no_supported_selection_decision_update
BEFORE UPDATE OF decision_action ON aircraft_identity_decisions
WHEN NEW.decision_action = 'no_supported_selection'
  AND EXISTS (
    SELECT 1
    FROM aircraft_identity_decision_claims claim
    WHERE claim.decision_id = OLD.id
  )
BEGIN
  SELECT RAISE(ABORT, 'decision with evidence claims cannot become no-supported-selection');
END;

CREATE TABLE IF NOT EXISTS aircraft_listing_identity_correction_decisions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  observation_id INTEGER NOT NULL
    REFERENCES aircraft_identity_observations(id) ON DELETE RESTRICT,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  correction_kind TEXT NOT NULL CHECK (correction_kind IN (
    'visual_identifier', 'faa_serial', 'publisher_hierarchy'
  )),
  expected_state_sha256 TEXT NOT NULL,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  prior_registration_number TEXT,
  prior_serial_number TEXT,
  corrected_registration_number TEXT,
  corrected_serial_number TEXT,
  faa_registry_snapshot_id INTEGER
    REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_source_record_sha256 TEXT,
  visual_resolution_json TEXT,
  decision_payload_json TEXT NOT NULL,
  decided_by_user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE RESTRICT,
  decided_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(expected_state_sha256) = 64 AND expected_state_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(rendered_html_sha256) = 64 AND rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (
    (correction_kind = 'visual_identifier'
      AND corrected_registration_number IS NOT NULL
      AND faa_registry_snapshot_id IS NOT NULL
      AND faa_source_record_sha256 IS NOT NULL
      AND visual_resolution_json IS NOT NULL)
    OR
    (correction_kind = 'faa_serial'
      AND corrected_registration_number IS NOT NULL
      AND corrected_serial_number IS NOT NULL
      AND faa_registry_snapshot_id IS NOT NULL
      AND faa_source_record_sha256 IS NOT NULL
      AND visual_resolution_json IS NULL)
    OR
    (correction_kind = 'publisher_hierarchy'
      AND faa_registry_snapshot_id IS NULL
      AND faa_source_record_sha256 IS NULL
      AND visual_resolution_json IS NULL
      AND corrected_registration_number IS prior_registration_number
      AND corrected_serial_number IS prior_serial_number)
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_listing_identity_corrections_listing
  ON aircraft_listing_identity_correction_decisions (
    aircraft_sale_listing_id, correction_kind, id
  );
CREATE UNIQUE INDEX IF NOT EXISTS uq_aircraft_listing_identity_correction_receipt
  ON aircraft_listing_identity_correction_decisions (
    plugin_submission_id, correction_kind
  );

DROP TRIGGER IF EXISTS aircraft_listing_identity_corrections_immutable_update;
CREATE TRIGGER aircraft_listing_identity_corrections_immutable_update
BEFORE UPDATE ON aircraft_listing_identity_correction_decisions
BEGIN SELECT RAISE(ABORT, 'aircraft listing identity correction decisions are immutable'); END;
DROP TRIGGER IF EXISTS aircraft_listing_identity_corrections_immutable_delete;
CREATE TRIGGER aircraft_listing_identity_corrections_immutable_delete
BEFORE DELETE ON aircraft_listing_identity_correction_decisions
BEGIN SELECT RAISE(ABORT, 'aircraft listing identity correction decisions are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_identity_correction_observation_immutable_update;
CREATE TRIGGER aircraft_identity_correction_observation_immutable_update
BEFORE UPDATE ON aircraft_identity_observations
WHEN EXISTS (
  SELECT 1 FROM aircraft_listing_identity_correction_decisions decision
  WHERE decision.observation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'aircraft identity observations referenced by correction decisions are immutable'); END;
DROP TRIGGER IF EXISTS aircraft_identity_correction_observation_immutable_delete;
CREATE TRIGGER aircraft_identity_correction_observation_immutable_delete
BEFORE DELETE ON aircraft_identity_observations
WHEN EXISTS (
  SELECT 1 FROM aircraft_listing_identity_correction_decisions decision
  WHERE decision.observation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'aircraft identity observations referenced by correction decisions are immutable'); END;

CREATE TABLE IF NOT EXISTS aircraft_source_visual_correction_artifacts (
  plugin_submission_id INTEGER PRIMARY KEY REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  observed_registration_number TEXT NOT NULL,
  corrected_registration_number TEXT NOT NULL,
  corrected_serial_number TEXT,
  faa_registry_snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_snapshot_archive_sha256 TEXT NOT NULL,
  faa_source_record_sha256 TEXT NOT NULL,
  primary_photo_asset_id TEXT NOT NULL,
  primary_photo_url TEXT NOT NULL,
  primary_photo_sha256 TEXT NOT NULL,
  visual_resolution_sha256 TEXT NOT NULL,
  visual_resolution_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(rendered_html_sha256) = 64 AND rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(faa_snapshot_archive_sha256) = 64 AND faa_snapshot_archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(faa_source_record_sha256) = 64 AND faa_source_record_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(primary_photo_sha256) = 64 AND primary_photo_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(visual_resolution_sha256) = 64 AND visual_resolution_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (observed_registration_number <> corrected_registration_number),
  CHECK (length(observed_registration_number) BETWEEN 2 AND 6),
  CHECK (length(corrected_registration_number) BETWEEN 2 AND 6),
  CHECK (corrected_serial_number IS NULL OR length(corrected_serial_number) BETWEEN 1 AND 128),
  CHECK (length(primary_photo_asset_id) BETWEEN 1 AND 256),
  CHECK (length(primary_photo_url) BETWEEN 1 AND 4096),
  CHECK (length(visual_resolution_json) BETWEEN 2 AND 65536 AND json_valid(visual_resolution_json) AND json_type(visual_resolution_json) = 'object'),
  FOREIGN KEY (faa_registry_snapshot_id, observed_registration_number) REFERENCES faa_registry_coverage(snapshot_id, n_number) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, corrected_registration_number) REFERENCES faa_registry_aircraft(snapshot_id, n_number) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_source_record_sha256) REFERENCES faa_registry_aircraft(snapshot_id, source_record_sha256) ON DELETE RESTRICT
);
DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_validate_insert;
CREATE TRIGGER aircraft_source_visual_artifacts_validate_insert
BEFORE INSERT ON aircraft_source_visual_correction_artifacts
WHEN NOT EXISTS (
  SELECT 1 FROM plugin_submissions submission
  JOIN faa_registry_snapshots snapshot ON snapshot.id = NEW.faa_registry_snapshot_id
  JOIN faa_registry_coverage observed ON observed.snapshot_id = snapshot.id AND observed.n_number = NEW.observed_registration_number AND observed.lookup_status = 'absent'
  JOIN faa_registry_coverage corrected ON corrected.snapshot_id = snapshot.id AND corrected.n_number = NEW.corrected_registration_number AND corrected.lookup_status = 'matched'
  JOIN faa_registry_aircraft aircraft ON aircraft.snapshot_id = snapshot.id AND aircraft.n_number = corrected.n_number
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.rendered_html_sha256
    AND snapshot.id = (SELECT id FROM faa_registry_snapshots ORDER BY snapshot_date DESC, id DESC LIMIT 1)
    AND snapshot.archive_sha256 = NEW.faa_snapshot_archive_sha256
    AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
    AND aircraft.manufacturer_serial_raw IS NEW.corrected_serial_number
)
BEGIN SELECT RAISE(ABORT, 'source visual correction artifact requires one exact current FAA absence/match pair'); END;
DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_immutable_update;
CREATE TRIGGER aircraft_source_visual_artifacts_immutable_update
BEFORE UPDATE ON aircraft_source_visual_correction_artifacts
BEGIN SELECT RAISE(ABORT, 'aircraft source visual correction artifacts are immutable'); END;
DROP TRIGGER IF EXISTS aircraft_source_visual_artifacts_immutable_delete;
CREATE TRIGGER aircraft_source_visual_artifacts_immutable_delete
BEFORE DELETE ON aircraft_source_visual_correction_artifacts
BEGIN SELECT RAISE(ABORT, 'aircraft source visual correction artifacts are immutable'); END;

DROP TRIGGER IF EXISTS aircraft_source_identity_receipt_gate;
CREATE TRIGGER aircraft_source_identity_receipt_gate
BEFORE UPDATE OF ingestion_state, ingestion_error, is_verified
ON aircraft_sale_listings
WHEN OLD.ingestion_error = 'source_identity_correction_receipt_pending'
 AND (
   NEW.ingestion_error IS NOT OLD.ingestion_error
   OR NEW.ingestion_state IS NOT OLD.ingestion_state
   OR NEW.is_verified IS NOT OLD.is_verified
 )
 AND NOT EXISTS (
   SELECT 1
   FROM aircraft_listing_identity_correction_decisions decision
   JOIN plugin_submissions submission
     ON submission.id = decision.plugin_submission_id
   WHERE decision.aircraft_sale_listing_id = OLD.id
     AND decision.correction_kind IN ('faa_serial', 'visual_identifier')
     AND decision.rendered_html_sha256 = submission.rendered_html_sha256
     AND submission.user_id = OLD.created_by_user_id
     AND submission.canonical_listing_id = OLD.id
     AND submission.extraction_error IS NULL
     AND NEW.registration_number IS decision.corrected_registration_number
     AND NEW.serial_number IS decision.corrected_serial_number
 )
BEGIN SELECT RAISE(ABORT, 'source identity correction receipt is required before leaving the receipt gate'); END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260821_aircraft_visual_source_corrections', 1,
  'ccc63aa23f2579ec5cec682bf1493a13eb73829718936b5890bd84de51bb828a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_aircraft_listing_identity_corrections',
  1,
  '589a0716726d2ffd34bf84c08583198383c003228b769c88f094ac6bd9f677b8',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

CREATE TABLE IF NOT EXISTS aircraft_reference_profile_proposals (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  resolution_case_id INTEGER NOT NULL
    REFERENCES aircraft_identity_resolution_cases(id) ON DELETE CASCADE,
  proposed_identity_json TEXT NOT NULL,
  proposed_profile_json TEXT NOT NULL,
  deterministic_validation_json TEXT NOT NULL,
  validation_status TEXT NOT NULL CHECK (validation_status IN (
    'pending', 'valid', 'invalid', 'needs_review'
  )),
  catalog_revision TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- Engine and propeller identities used by reference profiles live in a clean,
-- approved-by-construction catalog. Legacy engine_models/propeller_models rows
-- remain outside this trusted boundary until individually curated.
CREATE TABLE IF NOT EXISTS aircraft_engine_catalog_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manufacturer_name TEXT NOT NULL,
  normalized_manufacturer_name TEXT NOT NULL,
  model_name TEXT NOT NULL,
  normalized_model_name TEXT NOT NULL,
  identifier_authority TEXT NOT NULL,
  normalized_identifier_authority TEXT NOT NULL,
  identifier_kind TEXT NOT NULL CHECK (identifier_kind IN (
    'manufacturer_model_code', 'regulator_model_designation',
    'manufacturer_part_number'
  )),
  authoritative_identifier TEXT NOT NULL,
  normalized_authoritative_identifier TEXT NOT NULL,
  catalog_status TEXT NOT NULL DEFAULT 'approved'
    CHECK (catalog_status = 'approved'),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(manufacturer_name)) > 0),
  CHECK (length(trim(normalized_manufacturer_name)) > 0),
  CHECK (length(trim(model_name)) > 0),
  CHECK (length(trim(normalized_model_name)) > 0),
  CHECK (length(trim(identifier_authority)) > 0),
  CHECK (length(trim(normalized_identifier_authority)) > 0),
  CHECK (length(trim(authoritative_identifier)) > 0),
  CHECK (length(trim(normalized_authoritative_identifier)) > 0),
  UNIQUE (normalized_manufacturer_name, normalized_model_name),
  UNIQUE (
    normalized_identifier_authority, identifier_kind,
    normalized_authoritative_identifier
  )
);

CREATE TABLE IF NOT EXISTS aircraft_propeller_catalog_models (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manufacturer_name TEXT NOT NULL,
  normalized_manufacturer_name TEXT NOT NULL,
  model_name TEXT NOT NULL,
  normalized_model_name TEXT NOT NULL,
  identifier_authority TEXT NOT NULL,
  normalized_identifier_authority TEXT NOT NULL,
  identifier_kind TEXT NOT NULL CHECK (identifier_kind IN (
    'manufacturer_model_code', 'regulator_model_designation',
    'manufacturer_part_number'
  )),
  authoritative_identifier TEXT NOT NULL,
  normalized_authoritative_identifier TEXT NOT NULL,
  catalog_status TEXT NOT NULL DEFAULT 'approved'
    CHECK (catalog_status = 'approved'),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(manufacturer_name)) > 0),
  CHECK (length(trim(normalized_manufacturer_name)) > 0),
  CHECK (length(trim(model_name)) > 0),
  CHECK (length(trim(normalized_model_name)) > 0),
  CHECK (length(trim(identifier_authority)) > 0),
  CHECK (length(trim(normalized_identifier_authority)) > 0),
  CHECK (length(trim(authoritative_identifier)) > 0),
  CHECK (length(trim(normalized_authoritative_identifier)) > 0),
  UNIQUE (normalized_manufacturer_name, normalized_model_name),
  UNIQUE (
    normalized_identifier_authority, identifier_kind,
    normalized_authoritative_identifier
  )
);

CREATE TABLE IF NOT EXISTS aircraft_markets (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  code TEXT NOT NULL UNIQUE,
  name TEXT NOT NULL,
  parent_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(code)) > 0),
  CHECK (parent_market_id IS NULL OR parent_market_id <> id)
);

INSERT INTO aircraft_markets (code, name)
VALUES ('GLOBAL', 'Global')
ON CONFLICT (code) DO NOTHING;

CREATE TABLE IF NOT EXISTS aircraft_makes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL UNIQUE,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(name)) > 0),
  CHECK (length(trim(normalized_name)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_model_families (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_make_id, normalized_name),
  UNIQUE (id, aircraft_make_id),
  CHECK (length(trim(name)) > 0),
  CHECK (length(trim(normalized_name)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_designations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE RESTRICT,
  official_designation TEXT NOT NULL,
  normalized_official_designation TEXT NOT NULL,
  display_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_official_designation),
  UNIQUE (id, aircraft_model_family_id),
  CHECK (length(trim(official_designation)) > 0),
  CHECK (length(trim(normalized_official_designation)) > 0),
  CHECK (length(trim(display_name)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_make_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_make_id, normalized_alias, aircraft_market_id),
  CHECK (valid_from_model_year IS NULL OR valid_from_model_year BETWEEN 1900 AND 2200),
  CHECK (valid_to_model_year IS NULL OR valid_to_model_year BETWEEN 1900 AND 2200),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE TABLE IF NOT EXISTS aircraft_family_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_alias, aircraft_market_id),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE TABLE IF NOT EXISTS aircraft_designation_aliases (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  alias TEXT NOT NULL,
  normalized_alias TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_designation_id, normalized_alias, aircraft_market_id),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE TABLE IF NOT EXISTS aircraft_designation_identifiers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  authority TEXT NOT NULL,
  identifier_kind TEXT NOT NULL CHECK (identifier_kind IN (
    'manufacturer_model_code', 'type_certificate_model',
    'type_certificate_number', 'icao_type_designator', 'other_authoritative'
  )),
  identifier_value TEXT NOT NULL,
  normalized_identifier_value TEXT NOT NULL,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  aircraft_market_id INTEGER REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_designation_id, authority, identifier_kind,
    normalized_identifier_value, aircraft_market_id
  ),
  CHECK (length(trim(authority)) > 0),
  CHECK (length(trim(normalized_identifier_value)) > 0),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

-- SQLite considers NULL values distinct inside a UNIQUE constraint. These
-- expression indexes keep unscoped aliases/identifiers unique as well.
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_make_aliases_scope
  ON aircraft_make_aliases (
    aircraft_make_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_family_aliases_scope
  ON aircraft_family_aliases (
    aircraft_model_family_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_designation_aliases_scope
  ON aircraft_designation_aliases (
    aircraft_designation_id, normalized_alias, coalesce(aircraft_market_id, 0)
  );
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_designation_identifiers_scope
  ON aircraft_designation_identifiers (
    aircraft_designation_id, authority, identifier_kind,
    normalized_identifier_value, coalesce(aircraft_market_id, 0)
  );

CREATE TABLE IF NOT EXISTS aircraft_generations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  ordinal INTEGER CHECK (ordinal IS NULL OR ordinal >= 0),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_name),
  UNIQUE (id, aircraft_model_family_id)
);

CREATE TABLE IF NOT EXISTS aircraft_generation_designations (
  aircraft_generation_id INTEGER NOT NULL
    REFERENCES aircraft_generations(id) ON DELETE CASCADE,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (aircraft_generation_id, aircraft_designation_id)
);

CREATE TABLE IF NOT EXISTS aircraft_factory_packages (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL
    REFERENCES aircraft_model_families(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalized_name TEXT NOT NULL,
  package_kind TEXT NOT NULL CHECK (package_kind IN (
    'trim_tier', 'option_bundle', 'special_edition'
  )),
  exclusivity_group TEXT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_model_family_id, normalized_name),
  UNIQUE (id, aircraft_model_family_id),
  CHECK (package_kind <> 'trim_tier' OR length(trim(exclusivity_group)) > 0)
);

CREATE TABLE IF NOT EXISTS aircraft_package_applicability (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_factory_package_id INTEGER NOT NULL
    REFERENCES aircraft_factory_packages(id) ON DELETE CASCADE,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE CASCADE,
  aircraft_generation_id INTEGER
    REFERENCES aircraft_generations(id) ON DELETE CASCADE,
  valid_from_model_year INTEGER,
  valid_to_model_year INTEGER,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_factory_package_id, aircraft_designation_id,
    aircraft_generation_id, valid_from_model_year, valid_to_model_year
  ),
  CHECK (
    valid_from_model_year IS NULL OR valid_to_model_year IS NULL
    OR valid_to_model_year >= valid_from_model_year
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_package_applicability_scope
  ON aircraft_package_applicability (
    aircraft_factory_package_id, aircraft_designation_id,
    coalesce(aircraft_generation_id, 0),
    coalesce(valid_from_model_year, 0), coalesce(valid_to_model_year, 0)
  );

CREATE TABLE IF NOT EXISTS aircraft_reference_configurations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_model_family_id INTEGER NOT NULL,
  aircraft_designation_id INTEGER NOT NULL,
  aircraft_generation_id INTEGER,
  tier_package_id INTEGER,
  configuration_kind TEXT NOT NULL CHECK (configuration_kind IN ('base', 'tier')),
  display_name TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (aircraft_designation_id, aircraft_model_family_id)
    REFERENCES aircraft_designations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_generation_id, aircraft_model_family_id)
    REFERENCES aircraft_generations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (tier_package_id, aircraft_model_family_id)
    REFERENCES aircraft_factory_packages(id, aircraft_model_family_id) ON DELETE RESTRICT,
  CHECK (
    (configuration_kind = 'base' AND tier_package_id IS NULL)
    OR (configuration_kind = 'tier' AND tier_package_id IS NOT NULL)
  )
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_base_no_generation
  ON aircraft_reference_configurations (aircraft_designation_id)
  WHERE configuration_kind = 'base' AND aircraft_generation_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_base_generation
  ON aircraft_reference_configurations (aircraft_designation_id, aircraft_generation_id)
  WHERE configuration_kind = 'base' AND aircraft_generation_id IS NOT NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_tier_no_generation
  ON aircraft_reference_configurations (aircraft_designation_id, tier_package_id)
  WHERE configuration_kind = 'tier' AND aircraft_generation_id IS NULL;
CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_reference_config_tier_generation
  ON aircraft_reference_configurations (
    aircraft_designation_id, aircraft_generation_id, tier_package_id
  )
  WHERE configuration_kind = 'tier' AND aircraft_generation_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS aircraft_serial_number_schemes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_make_id INTEGER NOT NULL REFERENCES aircraft_makes(id) ON DELETE RESTRICT,
  name TEXT NOT NULL,
  normalization_version TEXT NOT NULL,
  validation_pattern TEXT NOT NULL,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_make_id, name, normalization_version)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_configuration_versions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configurations(id) ON DELETE RESTRICT,
  model_year INTEGER NOT NULL CHECK (model_year BETWEEN 1900 AND 2200),
  revision INTEGER NOT NULL CHECK (revision >= 1),
  supersedes_version_id INTEGER
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE RESTRICT,
  publication_state TEXT NOT NULL DEFAULT 'building'
    CHECK (publication_state IN ('building', 'published', 'superseded')),
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  published_at TEXT,
  superseded_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_id, model_year, revision),
  UNIQUE (supersedes_version_id),
  CHECK (supersedes_version_id IS NULL OR supersedes_version_id <> id),
  CHECK (
    (publication_state = 'building' AND published_at IS NULL AND superseded_at IS NULL)
    OR (publication_state = 'published' AND published_at IS NOT NULL AND superseded_at IS NULL)
    OR (publication_state = 'superseded' AND published_at IS NOT NULL AND superseded_at IS NOT NULL)
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_reference_versions_lookup
  ON aircraft_reference_configuration_versions (
    aircraft_reference_configuration_id, model_year, publication_state, revision
  );

CREATE TABLE IF NOT EXISTS aircraft_reference_applicability_scopes (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_market_id INTEGER NOT NULL REFERENCES aircraft_markets(id) ON DELETE RESTRICT,
  applies_to_all_serials INTEGER NOT NULL DEFAULT 1
    CHECK (applies_to_all_serials IN (0, 1)),
  aircraft_serial_number_scheme_id INTEGER
    REFERENCES aircraft_serial_number_schemes(id) ON DELETE RESTRICT,
  serial_prefix TEXT,
  serial_from_display TEXT,
  serial_to_display TEXT,
  serial_from_sort_key TEXT,
  serial_to_sort_key TEXT,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (
    (applies_to_all_serials = 1
      AND aircraft_serial_number_scheme_id IS NULL
      AND serial_prefix IS NULL
      AND serial_from_display IS NULL AND serial_to_display IS NULL
      AND serial_from_sort_key IS NULL AND serial_to_sort_key IS NULL)
    OR
    (applies_to_all_serials = 0
      AND aircraft_serial_number_scheme_id IS NOT NULL
      AND serial_from_display IS NOT NULL AND serial_to_display IS NOT NULL
      AND serial_from_sort_key IS NOT NULL AND serial_to_sort_key IS NOT NULL
      AND serial_from_sort_key <= serial_to_sort_key)
  ),
  UNIQUE (
    aircraft_reference_configuration_version_id, aircraft_market_id,
    aircraft_serial_number_scheme_id, serial_prefix,
    serial_from_sort_key, serial_to_sort_key
  )
);

CREATE INDEX IF NOT EXISTS idx_aircraft_reference_scope_market
  ON aircraft_reference_applicability_scopes (
    aircraft_market_id, aircraft_serial_number_scheme_id,
    serial_from_sort_key, serial_to_sort_key
  );

-- Recompute every stored bound from its canonical display value. This view is
-- also used by the insert trigger so direct SQL cannot create a second,
-- caller-defined ordering domain.
CREATE VIEW IF NOT EXISTS aircraft_reference_serial_key_errors AS
WITH RECURSIVE
bounds(scope_id, bound_name, serial_value, stored_key) AS (
  SELECT id, 'from', serial_from_display, serial_from_sort_key
  FROM aircraft_reference_applicability_scopes
  WHERE applies_to_all_serials = 0
  UNION ALL
  SELECT id, 'to', serial_to_display, serial_to_sort_key
  FROM aircraft_reference_applicability_scopes
  WHERE applies_to_all_serials = 0
),
state(
  scope_id, bound_name, serial_value, stored_key,
  position, segment, alpha_hex, numeric_segment, encoded
) AS (
  SELECT
    scope_id, bound_name, serial_value, stored_key, 2,
    substr(serial_value, 1, 1),
    CASE WHEN substr(serial_value, 1, 1) GLOB '[0-9]' THEN ''
      ELSE printf('%02X', instr(
        'ABCDEFGHIJKLMNOPQRSTUVWXYZ', substr(serial_value, 1, 1)
      )) END,
    substr(serial_value, 1, 1) GLOB '[0-9]', '01'
  FROM bounds
  UNION ALL
  SELECT
    scope_id, bound_name, serial_value, stored_key, position + 1,
    CASE WHEN (substr(serial_value, position, 1) GLOB '[0-9]') = numeric_segment
      THEN segment || substr(serial_value, position, 1)
      ELSE substr(serial_value, position, 1) END,
    CASE WHEN (substr(serial_value, position, 1) GLOB '[0-9]') = numeric_segment
      THEN alpha_hex || CASE WHEN numeric_segment THEN '' ELSE printf(
        '%02X', instr('ABCDEFGHIJKLMNOPQRSTUVWXYZ', substr(serial_value, position, 1))
      ) END
      ELSE CASE WHEN substr(serial_value, position, 1) GLOB '[0-9]' THEN ''
        ELSE printf('%02X', instr(
          'ABCDEFGHIJKLMNOPQRSTUVWXYZ', substr(serial_value, position, 1)
        )) END END,
    substr(serial_value, position, 1) GLOB '[0-9]',
    CASE WHEN (substr(serial_value, position, 1) GLOB '[0-9]') = numeric_segment
      THEN encoded
      ELSE encoded || CASE WHEN numeric_segment THEN
        '20'
        || printf('%08X', length(CASE WHEN trim(segment, '0') = ''
          THEN '0' ELSE ltrim(segment, '0') END))
        || CASE WHEN trim(segment, '0') = '' THEN '0' ELSE ltrim(segment, '0') END
        || printf('%08X', length(segment)) || segment
      ELSE '10' || alpha_hex || '00' END END
  FROM state
  WHERE position <= length(serial_value)
),
expected(scope_id, bound_name, expected_key) AS (
  SELECT scope_id, bound_name,
    encoded || CASE WHEN numeric_segment THEN
      '20'
      || printf('%08X', length(CASE WHEN trim(segment, '0') = ''
        THEN '0' ELSE ltrim(segment, '0') END))
      || CASE WHEN trim(segment, '0') = '' THEN '0' ELSE ltrim(segment, '0') END
      || printf('%08X', length(segment)) || segment
    ELSE '10' || alpha_hex || '00' END || '00'
  FROM state
  WHERE position = length(serial_value) + 1
)
SELECT
  bounds.scope_id, bounds.bound_name, bounds.serial_value,
  bounds.stored_key, expected.expected_key
FROM bounds
LEFT JOIN expected
  ON expected.scope_id = bounds.scope_id
 AND expected.bound_name = bounds.bound_name
WHERE bounds.serial_value IS NULL
   OR bounds.serial_value = ''
   OR bounds.serial_value <> upper(bounds.serial_value)
   OR bounds.serial_value GLOB '*[^A-Z0-9]*'
   OR expected.expected_key IS NULL
   OR bounds.stored_key IS NOT expected.expected_key;

CREATE TABLE IF NOT EXISTS aircraft_reference_prices (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  price_kind TEXT NOT NULL CHECK (price_kind IN (
    'base_msrp', 'equipped_msrp', 'tier_increment', 'other_factory_price'
  )),
  amount REAL NOT NULL CHECK (amount > 0),
  currency TEXT NOT NULL CHECK (length(currency) = 3 AND currency = upper(currency)),
  price_reference_year INTEGER NOT NULL CHECK (price_reference_year BETWEEN 1900 AND 2200),
  configuration_basis TEXT NOT NULL DEFAULT 'unknown' CHECK (configuration_basis IN (
    'full_standard_configuration', 'base_aircraft_only', 'unknown'
  )),
  evidence_kind TEXT NOT NULL CHECK (evidence_kind IN (
    'direct_model_year', 'direct_other_year', 'interpolated', 'inferred'
  )),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, price_kind, currency)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_avionics (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  avionics_model_id INTEGER NOT NULL REFERENCES avionics_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL CHECK (quantity > 0),
  equipment_role TEXT NOT NULL CHECK (equipment_role IN ('standard', 'included_in_tier')),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, avionics_model_id)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_engines (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_engine_catalog_model_id INTEGER NOT NULL
    REFERENCES aircraft_engine_catalog_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL CHECK (quantity > 0),
  equipment_role TEXT NOT NULL CHECK (equipment_role IN ('standard', 'included_in_tier')),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_reference_configuration_version_id,
    aircraft_engine_catalog_model_id
  )
);

CREATE TABLE IF NOT EXISTS aircraft_reference_propellers (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_propeller_catalog_model_id INTEGER NOT NULL
    REFERENCES aircraft_propeller_catalog_models(id) ON DELETE RESTRICT,
  quantity INTEGER NOT NULL CHECK (quantity > 0),
  equipment_role TEXT NOT NULL CHECK (equipment_role IN ('standard', 'included_in_tier')),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_reference_configuration_version_id,
    aircraft_propeller_catalog_model_id
  )
);

CREATE TABLE IF NOT EXISTS aircraft_feature_definitions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  feature_key TEXT NOT NULL UNIQUE,
  display_name TEXT NOT NULL,
  value_type TEXT NOT NULL CHECK (value_type IN ('boolean', 'number', 'text')),
  canonical_unit TEXT,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK ((value_type = 'number') OR canonical_unit IS NULL)
);

CREATE TABLE IF NOT EXISTS aircraft_reference_features (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  aircraft_feature_definition_id INTEGER NOT NULL
    REFERENCES aircraft_feature_definitions(id) ON DELETE RESTRICT,
  boolean_value INTEGER CHECK (boolean_value IS NULL OR boolean_value IN (0, 1)),
  number_value REAL,
  text_value TEXT,
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (
    aircraft_reference_configuration_version_id,
    aircraft_feature_definition_id
  ),
  CHECK (
    (boolean_value IS NOT NULL) + (number_value IS NOT NULL) + (text_value IS NOT NULL) = 1
  )
);

CREATE TABLE IF NOT EXISTS aircraft_reference_fact_set_attestations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_reference_configuration_version_id INTEGER NOT NULL
    REFERENCES aircraft_reference_configuration_versions(id) ON DELETE CASCADE,
  fact_set_kind TEXT NOT NULL CHECK (fact_set_kind IN (
    'avionics', 'engines', 'propellers', 'features'
  )),
  evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (aircraft_reference_configuration_version_id, fact_set_kind)
);

CREATE TABLE IF NOT EXISTS official_dollar_normalization_facts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  source_year INTEGER NOT NULL CHECK (source_year BETWEEN 1900 AND 2200),
  target_year INTEGER NOT NULL CHECK (target_year BETWEEN 1900 AND 2200),
  index_series TEXT NOT NULL CHECK (length(trim(index_series)) > 0),
  source_index_value REAL NOT NULL CHECK (source_index_value > 0),
  target_index_value REAL NOT NULL CHECK (target_index_value > 0),
  normalization_factor REAL NOT NULL CHECK (normalization_factor > 0),
  evidence_claim_id INTEGER NOT NULL UNIQUE
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (source_year, target_year),
  CHECK (source_year <> target_year),
  CHECK (
    abs(normalization_factor - (target_index_value / source_index_value))
      <= 0.000000001
  )
);

CREATE TRIGGER IF NOT EXISTS official_dollar_normalization_require_evidence
BEFORE INSERT ON official_dollar_normalization_facts
WHEN NOT EXISTS (
  SELECT 1
  FROM curation_evidence_claims claim
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE claim.id = NEW.evidence_claim_id
    AND claim.validation_status = 'validated'
    AND claim.claim_kind IN ('price', 'specification')
    AND source.source_tier = 'regulator_primary'
)
BEGIN SELECT RAISE(ABORT, 'dollar normalization requires validated official regulator evidence'); END;
CREATE TRIGGER IF NOT EXISTS official_dollar_normalization_immutable_update
BEFORE UPDATE ON official_dollar_normalization_facts
BEGIN SELECT RAISE(ABORT, 'official dollar normalization facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS official_dollar_normalization_immutable_delete
BEFORE DELETE ON official_dollar_normalization_facts
BEGIN SELECT RAISE(ABORT, 'official dollar normalization facts are immutable'); END;

-- Component catalog entries require an exact validated primary-source
-- identifier claim linked to the matching approved decision.
CREATE TRIGGER IF NOT EXISTS aircraft_engine_catalog_models_require_approval
BEFORE INSERT ON aircraft_engine_catalog_models
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims claim
    ON claim.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'engine_model'
    AND decision_claim.evidence_claim_id = NEW.identity_evidence_claim_id
    AND decision_claim.evidence_role IN ('identity', 'specification')
    AND claim.claim_kind IN ('identity', 'specification')
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'engine catalog model requires an approved primary-source identifier');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_propeller_catalog_models_require_approval
BEFORE INSERT ON aircraft_propeller_catalog_models
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims claim
    ON claim.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'propeller_model'
    AND decision_claim.evidence_claim_id = NEW.identity_evidence_claim_id
    AND decision_claim.evidence_role IN ('identity', 'specification')
    AND claim.claim_kind IN ('identity', 'specification')
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'propeller catalog model requires an approved primary-source identifier');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_engine_catalog_models_immutable_update
BEFORE UPDATE ON aircraft_engine_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved engine catalog models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_engine_catalog_models_immutable_delete
BEFORE DELETE ON aircraft_engine_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved engine catalog models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_propeller_catalog_models_immutable_update
BEFORE UPDATE ON aircraft_propeller_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved propeller catalog models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_propeller_catalog_models_immutable_delete
BEFORE DELETE ON aircraft_propeller_catalog_models
BEGIN SELECT RAISE(ABORT, 'approved propeller catalog models are immutable'); END;

-- Every canonical aircraft identity/configuration row must be backed by one
-- approved decision with at least one validated primary-source identity claim.
CREATE TRIGGER IF NOT EXISTS aircraft_makes_require_approval
BEFORE INSERT ON aircraft_makes
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims claim
    ON claim.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'make'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft make requires an approved primary-source decision');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_families_require_approval
BEFORE INSERT ON aircraft_model_families
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'family'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft family requires an approved primary-source decision');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_designations_require_approval
BEFORE INSERT ON aircraft_designations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'designation'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft designation requires an approved primary-source decision');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_aliases_require_approval_make
BEFORE INSERT ON aircraft_make_aliases
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'alias'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'aircraft alias requires an approved evidence-backed decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_aliases_require_approval_family
BEFORE INSERT ON aircraft_family_aliases
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'alias'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'aircraft alias requires an approved evidence-backed decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_aliases_require_approval_designation
BEFORE INSERT ON aircraft_designation_aliases
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'alias'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'aircraft alias requires an approved evidence-backed decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_identifiers_require_approval
BEFORE INSERT ON aircraft_designation_identifiers
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'identifier'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN SELECT RAISE(ABORT, 'aircraft identifier requires an approved primary-source decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_generations_require_approval
BEFORE INSERT ON aircraft_generations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'generation'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN SELECT RAISE(ABORT, 'aircraft generation requires an approved primary-source decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_generation_designations_require_approval
BEFORE INSERT ON aircraft_generation_designations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions
  WHERE id = NEW.approval_decision_id
    AND decision_status = 'approved' AND decision_action = 'approve_new'
    AND entity_kind = 'generation_designation'
)
OR NOT EXISTS (
  SELECT 1
  FROM aircraft_generations generation
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  WHERE generation.id = NEW.aircraft_generation_id
    AND generation.aircraft_model_family_id = designation.aircraft_model_family_id
)
BEGIN SELECT RAISE(ABORT, 'generation/designation link requires approval within one family'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_packages_require_approval
BEFORE INSERT ON aircraft_factory_packages
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'package'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
BEGIN SELECT RAISE(ABORT, 'aircraft package requires an approved primary-source decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_package_applicability_require_approval
BEFORE INSERT ON aircraft_package_applicability
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions
  WHERE id = NEW.approval_decision_id
    AND decision_status = 'approved' AND decision_action = 'approve_new'
    AND entity_kind = 'package_applicability'
)
OR NOT EXISTS (
  SELECT 1
  FROM aircraft_factory_packages package
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  LEFT JOIN aircraft_generations generation
    ON generation.id = NEW.aircraft_generation_id
  WHERE package.id = NEW.aircraft_factory_package_id
    AND package.aircraft_model_family_id = designation.aircraft_model_family_id
    AND (
      NEW.aircraft_generation_id IS NULL
      OR generation.aircraft_model_family_id = designation.aircraft_model_family_id
    )
)
BEGIN SELECT RAISE(ABORT, 'package applicability requires approval within one family'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_reference_configurations_require_approval
BEFORE INSERT ON aircraft_reference_configurations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'reference_configuration'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
OR (
  NEW.aircraft_generation_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1 FROM aircraft_generation_designations link
    WHERE link.aircraft_generation_id = NEW.aircraft_generation_id
      AND link.aircraft_designation_id = NEW.aircraft_designation_id
  )
)
OR (
  NEW.tier_package_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_factory_packages package
    JOIN aircraft_package_applicability applicability
      ON applicability.aircraft_factory_package_id = package.id
    WHERE package.id = NEW.tier_package_id
      AND package.package_kind = 'trim_tier'
      AND applicability.aircraft_designation_id = NEW.aircraft_designation_id
      AND (
        applicability.aircraft_generation_id IS NULL
        OR applicability.aircraft_generation_id = NEW.aircraft_generation_id
      )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'reference configuration requires approved applicable identity dimensions');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_serial_schemes_require_approval
BEFORE INSERT ON aircraft_serial_number_schemes
WHEN NEW.normalization_version <> 'natural_alphanumeric_segments_v1'
OR NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new' AND decision.entity_kind = 'serial_scheme'
    AND claim.validation_status = 'validated'
)
BEGIN SELECT RAISE(ABORT, 'serial scheme requires the universal ordering and an approved evidence-backed decision'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_serial_schemes_preserve_ordering
BEFORE UPDATE OF normalization_version ON aircraft_serial_number_schemes
WHEN NEW.normalization_version <> 'natural_alphanumeric_segments_v1'
BEGIN SELECT RAISE(ABORT, 'serial scheme ordering version is immutable'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_feature_definitions_require_approval
BEFORE INSERT ON aircraft_feature_definitions
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions
  WHERE id = NEW.approval_decision_id
    AND decision_status = 'approved' AND decision_action = 'approve_new'
    AND entity_kind = 'feature_definition'
)
BEGIN SELECT RAISE(ABORT, 'feature definition requires an approved decision'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_reference_versions_require_approval
BEFORE INSERT ON aircraft_reference_configuration_versions
WHEN NEW.publication_state <> 'building'
OR NOT EXISTS (
  SELECT 1 FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims dc ON dc.decision_id = decision.id
  JOIN curation_evidence_claims claim ON claim.id = dc.evidence_claim_id
  JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
  WHERE decision.id = NEW.approval_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'reference_profile'
    AND claim.validation_status = 'validated'
    AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
)
OR (NEW.revision = 1) <> (NEW.supersedes_version_id IS NULL)
OR (
  NEW.supersedes_version_id IS NOT NULL
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_reference_configuration_versions previous
    WHERE previous.id = NEW.supersedes_version_id
      AND previous.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
      AND previous.model_year = NEW.model_year
      AND previous.revision = NEW.revision - 1
      AND previous.publication_state = 'published'
  )
)
BEGIN
  SELECT RAISE(ABORT, 'reference profile requires building state, approved evidence, and its exact predecessor');
END;

-- Profile children may only be assembled while the parent is building.
CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_building_insert
BEFORE INSERT ON aircraft_reference_applicability_scopes
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference profile children require a building version'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_canonical_insert
BEFORE INSERT ON aircraft_reference_applicability_scopes
WHEN NEW.applies_to_all_serials = 0 AND (
  NEW.serial_from_sort_key <> upper(NEW.serial_from_sort_key)
  OR NEW.serial_to_sort_key <> upper(NEW.serial_to_sort_key)
  OR NEW.serial_from_sort_key GLOB '*[^A-F0-9]*'
  OR NEW.serial_to_sort_key GLOB '*[^A-F0-9]*'
  OR substr(NEW.serial_from_sort_key, 1, 2) <> '01'
  OR substr(NEW.serial_to_sort_key, 1, 2) <> '01'
  OR substr(NEW.serial_from_sort_key, -2) <> '00'
  OR substr(NEW.serial_to_sort_key, -2) <> '00'
  OR NEW.serial_from_sort_key COLLATE BINARY
       > NEW.serial_to_sort_key COLLATE BINARY
  OR NOT EXISTS (
    SELECT 1 FROM aircraft_serial_number_schemes scheme
    WHERE scheme.id = NEW.aircraft_serial_number_scheme_id
      AND scheme.normalization_version = 'natural_alphanumeric_segments_v1'
  )
  OR (
    NEW.serial_prefix IS NOT NULL
    AND (
      NEW.serial_prefix <> upper(NEW.serial_prefix)
      OR NEW.serial_prefix GLOB '*[^A-Z0-9]*'
      OR substr(NEW.serial_from_display, 1, length(NEW.serial_prefix))
           <> NEW.serial_prefix
      OR substr(NEW.serial_to_display, 1, length(NEW.serial_prefix))
           <> NEW.serial_prefix
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'reference serial applicability requires the universal natural-order key');
END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_key_recompute_insert
AFTER INSERT ON aircraft_reference_applicability_scopes
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_serial_key_errors error
  WHERE error.scope_id = NEW.id
)
BEGIN SELECT RAISE(ABORT, 'reference serial sort keys must be recomputed from canonical display values'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_price_building_insert
BEFORE INSERT ON aircraft_reference_prices
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference price requires a building version'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_avionics_building_insert
BEFORE INSERT ON aircraft_reference_avionics
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM avionics_models model
  WHERE model.id = NEW.avionics_model_id AND model.catalog_status = 'approved'
)
BEGIN SELECT RAISE(ABORT, 'reference avionics requires a building version and approved product'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_engines_building_insert
BEFORE INSERT ON aircraft_reference_engines
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM aircraft_engine_catalog_models model
  WHERE model.id = NEW.aircraft_engine_catalog_model_id
    AND model.catalog_status = 'approved'
)
BEGIN SELECT RAISE(ABORT, 'reference engine requires a building version and approved catalog model'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_propellers_building_insert
BEFORE INSERT ON aircraft_reference_propellers
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM aircraft_propeller_catalog_models model
  WHERE model.id = NEW.aircraft_propeller_catalog_model_id
    AND model.catalog_status = 'approved'
)
BEGIN SELECT RAISE(ABORT, 'reference propeller requires a building version and approved catalog model'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_features_building_insert
BEFORE INSERT ON aircraft_reference_features
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
OR NOT EXISTS (
  SELECT 1 FROM aircraft_feature_definitions definition
  WHERE definition.id = NEW.aircraft_feature_definition_id
    AND (
      (definition.value_type = 'boolean' AND NEW.boolean_value IS NOT NULL)
      OR (definition.value_type = 'number' AND NEW.number_value IS NOT NULL)
      OR (definition.value_type = 'text' AND NEW.text_value IS NOT NULL)
    )
)
BEGIN SELECT RAISE(ABORT, 'reference feature value does not match its definition'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_fact_set_building_insert
BEFORE INSERT ON aircraft_reference_fact_set_attestations
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = NEW.aircraft_reference_configuration_version_id
    AND version.publication_state = 'building'
)
BEGIN SELECT RAISE(ABORT, 'reference fact-set attestation requires a building version'); END;

-- No profile fact can be changed after insertion. Correct data by publishing a
-- replacement version rather than mutating a historical configuration.
CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_immutable_update
BEFORE UPDATE ON aircraft_reference_applicability_scopes
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_price_immutable_update
BEFORE UPDATE ON aircraft_reference_prices
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_avionics_immutable_update
BEFORE UPDATE ON aircraft_reference_avionics
WHEN NOT (
  NEW.id = OLD.id
  AND NEW.aircraft_reference_configuration_version_id
    = OLD.aircraft_reference_configuration_version_id
  AND NEW.avionics_model_id IS NOT OLD.avionics_model_id
  AND NEW.quantity = OLD.quantity
  AND NEW.equipment_role = OLD.equipment_role
  AND NEW.evidence_claim_id = OLD.evidence_claim_id
  AND NEW.created_at = OLD.created_at
  AND EXISTS (
    SELECT 1
    FROM avionics_catalog_authorized_consolidations guard
    JOIN avionics_models survivor ON survivor.id = guard.survivor_model_id
    JOIN avionics_models legacy ON legacy.id = OLD.avionics_model_id
    WHERE guard.duplicate_model_id = OLD.avionics_model_id
      AND guard.survivor_model_id = NEW.avionics_model_id
  )
)
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_engines_immutable_update
BEFORE UPDATE ON aircraft_reference_engines
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_propellers_immutable_update
BEFORE UPDATE ON aircraft_reference_propellers
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_features_immutable_update
BEFORE UPDATE ON aircraft_reference_features
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_reference_scope_immutable_delete
BEFORE DELETE ON aircraft_reference_applicability_scopes
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_price_immutable_delete
BEFORE DELETE ON aircraft_reference_prices
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_avionics_immutable_delete
BEFORE DELETE ON aircraft_reference_avionics
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_engines_immutable_delete
BEFORE DELETE ON aircraft_reference_engines
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_propellers_immutable_delete
BEFORE DELETE ON aircraft_reference_propellers
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_features_immutable_delete
BEFORE DELETE ON aircraft_reference_features
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_fact_set_immutable_update
BEFORE UPDATE ON aircraft_reference_fact_set_attestations
BEGIN SELECT RAISE(ABORT, 'reference profile facts are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_reference_fact_set_immutable_delete
BEFORE DELETE ON aircraft_reference_fact_set_attestations
WHEN EXISTS (
  SELECT 1 FROM aircraft_reference_configuration_versions version
  WHERE version.id = OLD.aircraft_reference_configuration_version_id
    AND version.publication_state <> 'building'
)
BEGIN SELECT RAISE(ABORT, 'published reference profile facts are immutable'); END;

-- Publication requires a complete exact-year price and at least one applicable
-- market/serial scope. It also rejects overlap with any already-published
-- version of the same logical configuration and model year.
CREATE TRIGGER IF NOT EXISTS aircraft_reference_versions_publish
BEFORE UPDATE OF publication_state ON aircraft_reference_configuration_versions
WHEN NEW.publication_state = 'published'
BEGIN
  SELECT RAISE(ABORT, 'only a building reference profile can be published')
  WHERE OLD.publication_state <> 'building';
  SELECT RAISE(ABORT, 'published reference profile requires published_at')
  WHERE NEW.published_at IS NULL;
  SELECT RAISE(ABORT, 'published reference profile requires applicability')
  WHERE NOT EXISTS (
    SELECT 1 FROM aircraft_reference_applicability_scopes scope
    WHERE scope.aircraft_reference_configuration_version_id = NEW.id
  );
  SELECT RAISE(ABORT, 'published reference profile requires complete factory fact-set attestations')
  WHERE 4 <> (
    SELECT COUNT(*) FROM aircraft_reference_fact_set_attestations attestation
    WHERE attestation.aircraft_reference_configuration_version_id = NEW.id
  );
  SELECT RAISE(ABORT, 'published reference profile requires exactly one direct exact-model-year full-configuration equipped MSRP with primary price evidence')
  WHERE 1 <> (
    SELECT COUNT(*)
    FROM aircraft_reference_prices price
    JOIN curation_evidence_claims claim ON claim.id = price.evidence_claim_id
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE price.aircraft_reference_configuration_version_id = NEW.id
      AND price.currency = 'USD'
      AND price.price_kind = 'equipped_msrp'
      AND price.evidence_kind = 'direct_model_year'
      AND price.configuration_basis = 'full_standard_configuration'
      AND claim.claim_kind = 'price'
      AND claim.validation_status = 'validated'
      AND source.source_tier IN ('manufacturer_primary', 'regulator_primary')
  );
  SELECT RAISE(ABORT, 'published reference profile requires approved engine catalog models')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_engines engine
    LEFT JOIN aircraft_engine_catalog_models model
      ON model.id = engine.aircraft_engine_catalog_model_id
     AND model.catalog_status = 'approved'
    WHERE engine.aircraft_reference_configuration_version_id = NEW.id
      AND model.id IS NULL
  );
  SELECT RAISE(ABORT, 'published reference profile requires approved propeller catalog models')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_propellers propeller
    LEFT JOIN aircraft_propeller_catalog_models model
      ON model.id = propeller.aircraft_propeller_catalog_model_id
     AND model.catalog_status = 'approved'
    WHERE propeller.aircraft_reference_configuration_version_id = NEW.id
      AND model.id IS NULL
  );
  SELECT RAISE(ABORT, 'published reference profile facts require validated primary evidence')
  WHERE EXISTS (
    SELECT 1
    FROM (
      SELECT evidence_claim_id, 'applicability' AS evidence_domain FROM aircraft_reference_applicability_scopes
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id, 'price' FROM aircraft_reference_prices
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id, 'factory' FROM aircraft_reference_avionics
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id, 'factory' FROM aircraft_reference_engines
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id, 'factory' FROM aircraft_reference_propellers
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id, 'factory' FROM aircraft_reference_features
      WHERE aircraft_reference_configuration_version_id = NEW.id
      UNION ALL
      SELECT evidence_claim_id, 'factory' FROM aircraft_reference_fact_set_attestations
      WHERE aircraft_reference_configuration_version_id = NEW.id
    ) fact
    JOIN curation_evidence_claims claim ON claim.id = fact.evidence_claim_id
    JOIN curation_evidence_sources source ON source.id = claim.evidence_source_id
    WHERE claim.validation_status <> 'validated'
       OR source.source_tier NOT IN ('manufacturer_primary', 'regulator_primary')
       OR (fact.evidence_domain = 'applicability' AND claim.claim_kind <> 'applicability')
       OR (fact.evidence_domain = 'price' AND claim.claim_kind <> 'price')
       OR (fact.evidence_domain = 'factory' AND claim.claim_kind NOT IN (
         'standard_equipment', 'package_composition', 'specification'
       ))
  );
  SELECT RAISE(ABORT, 'reference profile contains overlapping applicability scopes')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_applicability_scopes left_scope
    JOIN aircraft_reference_applicability_scopes right_scope
      ON right_scope.aircraft_reference_configuration_version_id = left_scope.aircraft_reference_configuration_version_id
     AND right_scope.id > left_scope.id
     AND right_scope.aircraft_market_id = left_scope.aircraft_market_id
    WHERE left_scope.aircraft_reference_configuration_version_id = NEW.id
      AND (
        left_scope.applies_to_all_serials = 1
        OR right_scope.applies_to_all_serials = 1
        OR (left_scope.serial_from_sort_key COLLATE BINARY
              <= right_scope.serial_to_sort_key COLLATE BINARY
          AND right_scope.serial_from_sort_key COLLATE BINARY
              <= left_scope.serial_to_sort_key COLLATE BINARY)
      )
  );
  SELECT RAISE(ABORT, 'published reference profile applicability overlaps an existing version')
  WHERE EXISTS (
    SELECT 1
    FROM aircraft_reference_applicability_scopes candidate
    JOIN aircraft_markets candidate_market
      ON candidate_market.id = candidate.aircraft_market_id
    JOIN aircraft_reference_applicability_scopes existing
      ON existing.aircraft_market_id = candidate.aircraft_market_id
      OR candidate_market.code = 'GLOBAL'
      OR EXISTS (
        SELECT 1 FROM aircraft_markets existing_market
        WHERE existing_market.id = existing.aircraft_market_id
          AND existing_market.code = 'GLOBAL'
      )
    JOIN aircraft_reference_configuration_versions existing_version
      ON existing_version.id = existing.aircraft_reference_configuration_version_id
    WHERE candidate.aircraft_reference_configuration_version_id = NEW.id
      AND existing_version.id <> NEW.id
      AND existing_version.aircraft_reference_configuration_id = NEW.aircraft_reference_configuration_id
      AND existing_version.model_year = NEW.model_year
      AND existing_version.publication_state = 'published'
      AND (
        candidate.applies_to_all_serials = 1
        OR existing.applies_to_all_serials = 1
        OR (candidate.serial_from_sort_key COLLATE BINARY
              <= existing.serial_to_sort_key COLLATE BINARY
          AND existing.serial_from_sort_key COLLATE BINARY
              <= candidate.serial_to_sort_key COLLATE BINARY)
      )
  );
END;

CREATE TRIGGER IF NOT EXISTS aircraft_reference_versions_immutable
BEFORE UPDATE ON aircraft_reference_configuration_versions
WHEN OLD.publication_state IN ('published', 'superseded')
AND NOT (
  OLD.publication_state = 'published'
  AND NEW.publication_state = 'superseded'
  AND NEW.superseded_at IS NOT NULL
  AND NEW.id = OLD.id
  AND NEW.aircraft_reference_configuration_id = OLD.aircraft_reference_configuration_id
  AND NEW.model_year = OLD.model_year
  AND NEW.revision = OLD.revision
  AND NEW.approval_decision_id = OLD.approval_decision_id
  AND NEW.published_at = OLD.published_at
  AND NEW.supersedes_version_id IS OLD.supersedes_version_id
)
BEGIN SELECT RAISE(ABORT, 'published reference profile versions are immutable'); END;

-- Privacy-minimized, target-scoped FAA releasable-registry projections.
CREATE TABLE IF NOT EXISTS faa_registry_snapshots (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  evidence_source_id INTEGER NOT NULL
    REFERENCES curation_evidence_sources(id) ON DELETE RESTRICT,
  snapshot_date TEXT NOT NULL,
  source_url TEXT NOT NULL,
  archive_sha256 TEXT NOT NULL,
  source_manifest_sha256 TEXT NOT NULL,
  target_set_sha256 TEXT NOT NULL,
  master_member_name TEXT NOT NULL CHECK (master_member_name = 'MASTER.txt'),
  master_member_sha256 TEXT NOT NULL,
  aircraft_member_name TEXT NOT NULL CHECK (aircraft_member_name = 'ACFTREF.txt'),
  aircraft_member_sha256 TEXT NOT NULL,
  engine_member_name TEXT NOT NULL CHECK (engine_member_name = 'ENGINE.txt'),
  engine_member_sha256 TEXT NOT NULL,
  imported_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  record_hash_domain TEXT NOT NULL CHECK (
    record_hash_domain = 'aircost-faa-master-retained-aircraft-projection-v1'
  ),
  UNIQUE (archive_sha256, target_set_sha256),
  CHECK (snapshot_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  CHECK (source_url LIKE 'https://faa.gov/%' OR source_url LIKE 'https://%.faa.gov/%'),
  CHECK (length(archive_sha256) = 64 AND archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(source_manifest_sha256) = 64 AND source_manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(target_set_sha256) = 64 AND target_set_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(master_member_sha256) = 64 AND master_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(aircraft_member_sha256) = 64 AND aircraft_member_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(engine_member_sha256) = 64 AND engine_member_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS idx_faa_registry_snapshots_current
  ON faa_registry_snapshots (snapshot_date DESC, id DESC);

CREATE TRIGGER IF NOT EXISTS faa_registry_snapshots_require_exact_evidence
BEFORE INSERT ON faa_registry_snapshots
WHEN NOT EXISTS (
  SELECT 1 FROM curation_evidence_sources source
  WHERE source.id = NEW.evidence_source_id
    AND source.source_domain = 'faa.gov'
    AND source.source_tier = 'regulator_primary'
    AND source.source_url = NEW.source_url
    AND source.content_sha256 = NEW.archive_sha256
)
BEGIN SELECT RAISE(ABORT, 'FAA snapshot requires exact regulator evidence provenance'); END;

CREATE TABLE IF NOT EXISTS faa_registry_aircraft (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  n_number TEXT NOT NULL,
  manufacturer_serial_raw TEXT,
  manufacturer_serial_key TEXT,
  aircraft_code TEXT NOT NULL,
  engine_code TEXT,
  year_manufactured INTEGER,
  source_record_sha256 TEXT NOT NULL,
  PRIMARY KEY (snapshot_id, n_number),
  UNIQUE (snapshot_id, source_record_sha256),
  CHECK (substr(n_number, 1, 1) = 'N' AND length(n_number) BETWEEN 2 AND 6),
  CHECK (manufacturer_serial_raw IS NULL OR length(trim(manufacturer_serial_raw)) > 0),
  CHECK (manufacturer_serial_key IS NULL OR length(manufacturer_serial_key) > 0),
  CHECK (length(trim(aircraft_code)) > 0),
  CHECK (engine_code IS NULL OR length(trim(engine_code)) > 0),
  CHECK (year_manufactured IS NULL OR year_manufactured BETWEEN 1900 AND 2200),
  CHECK (length(source_record_sha256) = 64 AND source_record_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS idx_faa_registry_aircraft_code
  ON faa_registry_aircraft (snapshot_id, aircraft_code);
CREATE INDEX IF NOT EXISTS idx_faa_registry_engine_code
  ON faa_registry_aircraft (snapshot_id, engine_code);

CREATE TABLE IF NOT EXISTS faa_registry_aircraft_references (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  aircraft_code TEXT NOT NULL,
  manufacturer_name TEXT,
  model_name TEXT,
  aircraft_type_code TEXT,
  engine_type_code TEXT,
  category_code TEXT,
  certification_indicator_code TEXT,
  engine_count INTEGER CHECK (engine_count IS NULL OR engine_count >= 0),
  seat_count INTEGER CHECK (seat_count IS NULL OR seat_count >= 0),
  weight_class_code TEXT,
  cruise_speed_mph INTEGER CHECK (cruise_speed_mph IS NULL OR cruise_speed_mph >= 0),
  type_certificate_data_sheet TEXT,
  type_certificate_holder TEXT,
  PRIMARY KEY (snapshot_id, aircraft_code),
  CHECK (length(trim(aircraft_code)) > 0)
);

CREATE TABLE IF NOT EXISTS faa_registry_engine_references (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  engine_code TEXT NOT NULL,
  manufacturer_name TEXT,
  model_name TEXT,
  engine_type_code TEXT,
  horsepower INTEGER CHECK (horsepower IS NULL OR horsepower >= 0),
  thrust_pounds INTEGER CHECK (thrust_pounds IS NULL OR thrust_pounds >= 0),
  PRIMARY KEY (snapshot_id, engine_code),
  CHECK (length(trim(engine_code)) > 0)
);

CREATE TABLE IF NOT EXISTS faa_registry_coverage (
  snapshot_id INTEGER NOT NULL REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  n_number TEXT NOT NULL,
  lookup_status TEXT NOT NULL CHECK (lookup_status IN ('matched', 'absent')),
  PRIMARY KEY (snapshot_id, n_number),
  CHECK (substr(n_number, 1, 1) = 'N' AND length(n_number) BETWEEN 2 AND 6)
);

CREATE INDEX IF NOT EXISTS idx_faa_registry_coverage_lookup
  ON faa_registry_coverage (n_number, snapshot_id);

CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_references_reachable
BEFORE INSERT ON faa_registry_aircraft_references
WHEN NOT EXISTS (
  SELECT 1 FROM faa_registry_aircraft aircraft
  WHERE aircraft.snapshot_id = NEW.snapshot_id
    AND aircraft.aircraft_code = NEW.aircraft_code
)
BEGIN SELECT RAISE(ABORT, 'FAA aircraft reference must be reachable from a target match'); END;

CREATE TRIGGER IF NOT EXISTS faa_registry_engine_references_reachable
BEFORE INSERT ON faa_registry_engine_references
WHEN NOT EXISTS (
  SELECT 1 FROM faa_registry_aircraft aircraft
  WHERE aircraft.snapshot_id = NEW.snapshot_id
    AND aircraft.engine_code = NEW.engine_code
)
BEGIN SELECT RAISE(ABORT, 'FAA engine reference must be reachable from a target match'); END;

CREATE TRIGGER IF NOT EXISTS faa_registry_coverage_consistent
BEFORE INSERT ON faa_registry_coverage
WHEN (NEW.lookup_status = 'matched' AND NOT EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      ))
  OR (NEW.lookup_status = 'absent' AND EXISTS (
        SELECT 1 FROM faa_registry_aircraft aircraft
        WHERE aircraft.snapshot_id = NEW.snapshot_id AND aircraft.n_number = NEW.n_number
      ))
BEGIN SELECT RAISE(ABORT, 'FAA coverage must agree with its target match'); END;

CREATE TRIGGER IF NOT EXISTS faa_registry_snapshots_immutable_update
BEFORE UPDATE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_snapshots_immutable_delete
BEFORE DELETE ON faa_registry_snapshots
BEGIN SELECT RAISE(ABORT, 'FAA registry snapshots are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_immutable_update
BEFORE UPDATE ON faa_registry_aircraft
BEGIN SELECT RAISE(ABORT, 'FAA registry aircraft are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_immutable_delete
BEFORE DELETE ON faa_registry_aircraft
BEGIN SELECT RAISE(ABORT, 'FAA registry aircraft are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_references_immutable_update
BEFORE UPDATE ON faa_registry_aircraft_references
BEGIN SELECT RAISE(ABORT, 'FAA aircraft references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_aircraft_references_immutable_delete
BEFORE DELETE ON faa_registry_aircraft_references
BEGIN SELECT RAISE(ABORT, 'FAA aircraft references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_engine_references_immutable_update
BEFORE UPDATE ON faa_registry_engine_references
BEGIN SELECT RAISE(ABORT, 'FAA engine references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_engine_references_immutable_delete
BEFORE DELETE ON faa_registry_engine_references
BEGIN SELECT RAISE(ABORT, 'FAA engine references are immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_coverage_immutable_update
BEFORE UPDATE ON faa_registry_coverage
BEGIN SELECT RAISE(ABORT, 'FAA registry coverage is immutable'); END;
CREATE TRIGGER IF NOT EXISTS faa_registry_coverage_immutable_delete
BEFORE DELETE ON faa_registry_coverage
BEGIN SELECT RAISE(ABORT, 'FAA registry coverage is immutable'); END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260820_faa_record_hash_domain',
  1,
  'f124f573bf705da6c1e4b0a5c7a8df45ea5a4a5dc009a28eee012be42c691502',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;



-- Immutable assignment versions retain every approved correction. The small
-- current-pointer table is the only mutable state.
-- N-registered listings are evaluated in the United States market. Aliases
-- scoped to other markets are not identity evidence for this pipeline.
INSERT INTO aircraft_markets (code, name, parent_market_id)
SELECT 'US', 'United States', id
FROM aircraft_markets
WHERE code = 'GLOBAL'
ON CONFLICT (code) DO NOTHING;

CREATE TABLE IF NOT EXISTS aircraft_designation_faa_bindings (
  faa_snapshot_date TEXT NOT NULL,
  faa_archive_sha256 TEXT NOT NULL,
  faa_aircraft_code TEXT NOT NULL,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE RESTRICT,
  representative_faa_registry_snapshot_id INTEGER NOT NULL,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (representative_faa_registry_snapshot_id, faa_aircraft_code)
    REFERENCES faa_registry_aircraft_references(snapshot_id, aircraft_code)
    ON DELETE RESTRICT,
  CHECK (faa_snapshot_date GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'),
  CHECK (length(faa_archive_sha256) = 64 AND faa_archive_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(trim(faa_aircraft_code)) > 0),
  PRIMARY KEY (faa_snapshot_date, faa_archive_sha256, faa_aircraft_code)
);

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_identity_assignments (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  supersedes_assignment_id INTEGER UNIQUE,
  aircraft_make_id INTEGER NOT NULL,
  aircraft_model_family_id INTEGER NOT NULL,
  aircraft_designation_id INTEGER NOT NULL,
  aircraft_generation_id INTEGER,
  aircraft_factory_package_id INTEGER,
  identity_decision_id INTEGER NOT NULL
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  faa_registry_snapshot_id INTEGER NOT NULL
    REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_n_number TEXT NOT NULL,
  faa_source_record_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (id, aircraft_sale_listing_id),
  FOREIGN KEY (supersedes_assignment_id, aircraft_sale_listing_id)
    REFERENCES aircraft_sale_listing_identity_assignments(id, aircraft_sale_listing_id)
    ON DELETE CASCADE,
  FOREIGN KEY (aircraft_model_family_id, aircraft_make_id)
    REFERENCES aircraft_model_families(id, aircraft_make_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_designation_id, aircraft_model_family_id)
    REFERENCES aircraft_designations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_generation_id, aircraft_model_family_id)
    REFERENCES aircraft_generations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_factory_package_id, aircraft_model_family_id)
    REFERENCES aircraft_factory_packages(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_n_number)
    REFERENCES faa_registry_aircraft(snapshot_id, n_number) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_source_record_sha256)
    REFERENCES faa_registry_aircraft(snapshot_id, source_record_sha256) ON DELETE RESTRICT,
  CHECK (substr(faa_n_number, 1, 1) = 'N' AND length(faa_n_number) BETWEEN 2 AND 6),
  CHECK (
    length(faa_source_record_sha256) = 64
    AND faa_source_record_sha256 NOT GLOB '*[^0-9a-f]*'
  )
);

CREATE TABLE IF NOT EXISTS aircraft_sale_listing_current_identity_assignments (
  aircraft_sale_listing_id INTEGER PRIMARY KEY
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  identity_assignment_id INTEGER NOT NULL UNIQUE,
  selected_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (identity_assignment_id, aircraft_sale_listing_id)
    REFERENCES aircraft_sale_listing_identity_assignments(id, aircraft_sale_listing_id)
    ON DELETE CASCADE
);

CREATE INDEX IF NOT EXISTS idx_listing_identity_assignment_designation
  ON aircraft_sale_listing_identity_assignments (
    aircraft_designation_id, aircraft_generation_id, aircraft_factory_package_id
  );

-- SQLite has no built-in regular-expression replacement. These read-only
-- projections implement the same ASCII-alphanumeric identity key used by Rust
-- and PostgreSQL, so every punctuation character is ignored consistently.
CREATE VIEW IF NOT EXISTS aircraft_designation_identity_keys AS
WITH RECURSIVE designation_characters (
  aircraft_designation_id, source_value, character_position, identity_key
) AS (
  SELECT id, normalized_official_designation, 1, ''
  FROM aircraft_designations
  UNION ALL
  SELECT aircraft_designation_id, source_value, character_position + 1,
    identity_key || CASE
      WHEN lower(substr(source_value, character_position, 1)) GLOB '[a-z0-9]'
      THEN lower(substr(source_value, character_position, 1))
      ELSE ''
    END
  FROM designation_characters
  WHERE character_position <= length(source_value)
)
SELECT aircraft_designation_id, identity_key
FROM designation_characters
WHERE character_position > length(source_value);

CREATE VIEW IF NOT EXISTS faa_registry_aircraft_reference_identity_keys AS
WITH RECURSIVE reference_characters (
  faa_registry_snapshot_id, faa_aircraft_code, source_value,
  character_position, identity_key
) AS (
  SELECT snapshot_id, aircraft_code, coalesce(model_name, ''), 1, ''
  FROM faa_registry_aircraft_references
  UNION ALL
  SELECT faa_registry_snapshot_id, faa_aircraft_code, source_value,
    character_position + 1,
    identity_key || CASE
      WHEN lower(substr(source_value, character_position, 1)) GLOB '[a-z0-9]'
      THEN lower(substr(source_value, character_position, 1))
      ELSE ''
    END
  FROM reference_characters
  WHERE character_position <= length(source_value)
)
SELECT faa_registry_snapshot_id, faa_aircraft_code, identity_key
FROM reference_characters
WHERE character_position > length(source_value);

-- Alias keys are retrieval keys, not free-form evidence. Keep their stored
-- form deterministic, prevent overlapping scopes from resolving one FAA label
-- to two makes, and preserve approved aliases immutably.
CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_key_validate
BEFORE INSERT ON aircraft_make_aliases
WHEN NEW.normalized_alias = ''
  OR NEW.normalized_alias <> trim(NEW.normalized_alias)
  OR NEW.normalized_alias <> lower(NEW.normalized_alias)
  OR NEW.normalized_alias GLOB '*[^a-z0-9 ]*'
  OR instr(NEW.normalized_alias, '  ') > 0
  OR replace(NEW.normalized_alias, ' ', '') <>
     lower(replace(replace(replace(replace(replace(replace(replace(replace(
       replace(replace(trim(NEW.alias), ' ', ''), '-', ''), '.', ''), '/', ''),
       '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
BEGIN
  SELECT RAISE(ABORT, 'aircraft make alias requires its deterministic normalized retrieval key');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_collision
BEFORE INSERT ON aircraft_make_aliases
WHEN EXISTS (
  SELECT 1
  FROM aircraft_make_aliases existing_alias
  LEFT JOIN aircraft_markets existing_market
    ON existing_market.id = existing_alias.aircraft_market_id
  LEFT JOIN aircraft_markets new_market
    ON new_market.id = NEW.aircraft_market_id
  WHERE existing_alias.aircraft_make_id <> NEW.aircraft_make_id
    AND existing_alias.normalized_alias = NEW.normalized_alias
    AND (existing_alias.valid_to_model_year IS NULL
      OR NEW.valid_from_model_year IS NULL
      OR existing_alias.valid_to_model_year >= NEW.valid_from_model_year)
    AND (NEW.valid_to_model_year IS NULL
      OR existing_alias.valid_from_model_year IS NULL
      OR NEW.valid_to_model_year >= existing_alias.valid_from_model_year)
    AND (existing_alias.aircraft_market_id IS NULL
      OR NEW.aircraft_market_id IS NULL
      OR existing_alias.aircraft_market_id = NEW.aircraft_market_id
      OR existing_market.code = 'GLOBAL'
      OR new_market.code = 'GLOBAL')
)
OR EXISTS (
  SELECT 1 FROM aircraft_makes other_make
  WHERE other_make.id <> NEW.aircraft_make_id
    AND (
      other_make.normalized_name = NEW.normalized_alias
      OR lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(other_make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = replace(NEW.normalized_alias, ' ', '')
    )
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft make alias overlaps another canonical make in market/year scope');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_immutable_update
BEFORE UPDATE ON aircraft_make_aliases
BEGIN SELECT RAISE(ABORT, 'approved aircraft make aliases are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_make_alias_identity_immutable_delete
BEFORE DELETE ON aircraft_make_aliases
BEGIN SELECT RAISE(ABORT, 'approved aircraft make aliases are immutable'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_make_identity_alias_collision_insert
BEFORE INSERT ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_make_aliases alias
  WHERE lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
    = replace(alias.normalized_alias, ' ', '')
)
BEGIN SELECT RAISE(ABORT, 'canonical aircraft make collides with an approved alias'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_make_identity_alias_collision_update
BEFORE UPDATE OF name, normalized_name ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_make_aliases alias
  WHERE alias.aircraft_make_id <> OLD.id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      = replace(alias.normalized_alias, ' ', '')
)
BEGIN SELECT RAISE(ABORT, 'canonical aircraft make collides with an approved alias'); END;

-- Fail the upgrade instead of grandfathering ambiguous or mechanically
-- inconsistent aliases that could authorize the wrong FAA manufacturer.
CREATE TABLE IF NOT EXISTS aircraft_identity_alias_upgrade_guard (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
DELETE FROM aircraft_identity_alias_upgrade_guard;
INSERT INTO aircraft_identity_alias_upgrade_guard (valid)
SELECT 0
WHERE EXISTS (
  SELECT 1
  FROM aircraft_make_aliases alias
  WHERE alias.normalized_alias = ''
    OR alias.normalized_alias <> trim(alias.normalized_alias)
    OR alias.normalized_alias <> lower(alias.normalized_alias)
    OR alias.normalized_alias GLOB '*[^a-z0-9 ]*'
    OR instr(alias.normalized_alias, '  ') > 0
    OR replace(alias.normalized_alias, ' ', '') <>
       lower(replace(replace(replace(replace(replace(replace(replace(replace(
         replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''),
         '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
)
OR EXISTS (
  SELECT 1
  FROM aircraft_make_aliases left_alias
  JOIN aircraft_make_aliases right_alias
    ON right_alias.id > left_alias.id
   AND right_alias.aircraft_make_id <> left_alias.aircraft_make_id
   AND right_alias.normalized_alias = left_alias.normalized_alias
  LEFT JOIN aircraft_markets left_market
    ON left_market.id = left_alias.aircraft_market_id
  LEFT JOIN aircraft_markets right_market
    ON right_market.id = right_alias.aircraft_market_id
  WHERE (left_alias.valid_to_model_year IS NULL
      OR right_alias.valid_from_model_year IS NULL
      OR left_alias.valid_to_model_year >= right_alias.valid_from_model_year)
    AND (right_alias.valid_to_model_year IS NULL
      OR left_alias.valid_from_model_year IS NULL
      OR right_alias.valid_to_model_year >= left_alias.valid_from_model_year)
    AND (left_alias.aircraft_market_id IS NULL
      OR right_alias.aircraft_market_id IS NULL
      OR left_alias.aircraft_market_id = right_alias.aircraft_market_id
      OR left_market.code = 'GLOBAL'
      OR right_market.code = 'GLOBAL')
)
OR EXISTS (
  SELECT 1
  FROM aircraft_make_aliases alias
  JOIN aircraft_makes other_make
    ON other_make.id <> alias.aircraft_make_id
  WHERE lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(other_make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
    = replace(alias.normalized_alias, ' ', '')
);
DROP TABLE aircraft_identity_alias_upgrade_guard;

CREATE TRIGGER IF NOT EXISTS aircraft_designation_faa_binding_requires_provenance
BEFORE INSERT ON aircraft_designation_faa_bindings
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_designations designation
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_model_families family
    ON family.id = designation.aircraft_model_family_id
  JOIN aircraft_makes make
    ON make.id = family.aircraft_make_id
  JOIN aircraft_identity_decisions decision
    ON decision.id = designation.approval_decision_id
  JOIN curation_evidence_claims claim
    ON claim.id = NEW.identity_evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.representative_faa_registry_snapshot_id
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = snapshot.id
   AND reference.aircraft_code = NEW.faa_aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  WHERE designation.id = NEW.aircraft_designation_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'designation'
    AND claim.claim_kind = 'identity'
    AND claim.validation_status = 'validated'
    AND source.id = snapshot.evidence_source_id
    AND source.source_tier = 'regulator_primary'
    AND NEW.faa_snapshot_date = snapshot.snapshot_date
    AND NEW.faa_archive_sha256 = snapshot.archive_sha256
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      OR (
        EXISTS (
          SELECT 1 FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
        )
        AND NOT EXISTS (
          SELECT 1
          FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
            AND NOT EXISTS (
              SELECT 1
              FROM aircraft_make_aliases alias
              LEFT JOIN aircraft_markets market
                ON market.id = alias.aircraft_market_id
              WHERE alias.aircraft_make_id = make.id
                AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
                  = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
                AND (alias.aircraft_market_id IS NULL OR market.code IN ('GLOBAL', 'US'))
                AND (
                  (registered_aircraft.year_manufactured IS NULL
                    AND alias.valid_from_model_year IS NULL
                    AND alias.valid_to_model_year IS NULL)
                  OR (registered_aircraft.year_manufactured IS NOT NULL
                    AND (alias.valid_from_model_year IS NULL
                      OR alias.valid_from_model_year <= registered_aircraft.year_manufactured)
                    AND (alias.valid_to_model_year IS NULL
                      OR alias.valid_to_model_year >= registered_aircraft.year_manufactured))
                )
            )
        )
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'FAA aircraft code binding requires an exact approved designation, applicable manufacturer identity, and regulator evidence');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_designation_faa_binding_immutable_update
BEFORE UPDATE ON aircraft_designation_faa_bindings
BEGIN SELECT RAISE(ABORT, 'FAA aircraft code bindings are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_designation_faa_binding_immutable_delete
BEFORE DELETE ON aircraft_designation_faa_bindings
BEGIN SELECT RAISE(ABORT, 'FAA aircraft code bindings are immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_provenance
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_designations designation
  JOIN aircraft_identity_decisions decision
    ON decision.id = designation.approval_decision_id
  JOIN aircraft_identity_decision_claims decision_claim
    ON decision_claim.decision_id = decision.id
  JOIN curation_evidence_claims decision_evidence
    ON decision_evidence.id = decision_claim.evidence_claim_id
  JOIN curation_evidence_sources decision_source
    ON decision_source.id = decision_evidence.evidence_source_id
  JOIN curation_evidence_claims assignment_evidence
    ON assignment_evidence.id = NEW.identity_evidence_claim_id
  JOIN curation_evidence_sources assignment_source
    ON assignment_source.id = assignment_evidence.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.faa_registry_snapshot_id
  WHERE designation.id = NEW.aircraft_designation_id
    AND decision.id = NEW.identity_decision_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'designation'
    AND decision_claim.evidence_role = 'identity'
    AND decision_evidence.validation_status = 'validated'
    AND decision_source.source_tier IN ('manufacturer_primary', 'regulator_primary')
    AND assignment_evidence.claim_kind = 'identity'
    AND assignment_evidence.validation_status = 'validated'
    AND assignment_source.id = snapshot.evidence_source_id
    AND assignment_source.source_tier = 'regulator_primary'
)
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment requires immutable designation-decision and current FAA evidence provenance');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_faa_identity
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = NEW.faa_registry_snapshot_id
   AND aircraft.n_number = NEW.faa_n_number
   AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_snapshots registry_snapshot
    ON registry_snapshot.id = aircraft.snapshot_id
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = registry_snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = registry_snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id = designation.id
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_makes make
    ON make.id = NEW.aircraft_make_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND upper(replace(replace(trim(listing.registration_number), '-', ''), ' ', ''))
      = NEW.faa_n_number
    AND length(trim(reference.manufacturer_name)) > 0
    AND length(trim(reference.model_name)) > 0
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
            = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
          AND (alias.aircraft_market_id IS NULL OR market.code IN ('GLOBAL', 'US'))
          AND (alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= listing.model_year)
          AND (alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= listing.model_year)
      )
    )
    AND designation_key.identity_key = reference_key.identity_key
)
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment designation does not match the exact FAA aircraft identity');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_linear_history
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN (NEW.supersedes_assignment_id IS NULL AND EXISTS (
        SELECT 1 FROM aircraft_sale_listing_identity_assignments prior
        WHERE prior.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
      ))
  OR (NEW.supersedes_assignment_id IS NOT NULL AND NOT EXISTS (
        SELECT 1 FROM aircraft_sale_listing_current_identity_assignments current_assignment
        WHERE current_assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
          AND current_assignment.identity_assignment_id = NEW.supersedes_assignment_id
      ))
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment must extend the current immutable history');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_requires_applicable_dimensions
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN (NEW.aircraft_generation_id IS NOT NULL AND NOT EXISTS (
        SELECT 1
        FROM aircraft_generation_designations link
        WHERE link.aircraft_generation_id = NEW.aircraft_generation_id
          AND link.aircraft_designation_id = NEW.aircraft_designation_id
      ))
  OR (NEW.aircraft_factory_package_id IS NOT NULL AND NOT EXISTS (
        SELECT 1
        FROM aircraft_sale_listings listing
        JOIN aircraft_package_applicability applicability
          ON applicability.aircraft_factory_package_id = NEW.aircraft_factory_package_id
         AND applicability.aircraft_designation_id = NEW.aircraft_designation_id
        WHERE listing.id = NEW.aircraft_sale_listing_id
          AND (
            (NEW.aircraft_generation_id IS NULL
              AND applicability.aircraft_generation_id IS NULL)
            OR applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = NEW.aircraft_generation_id
          )
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= listing.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= listing.model_year)
      ))
  OR (NEW.aircraft_generation_id IS NULL AND EXISTS (
        SELECT 1
        FROM aircraft_generation_designations link
        WHERE link.aircraft_designation_id = NEW.aircraft_designation_id
      ))
  OR (NEW.aircraft_factory_package_id IS NULL AND EXISTS (
        SELECT 1
        FROM aircraft_sale_listings listing
        JOIN aircraft_package_applicability applicability
          ON applicability.aircraft_designation_id = NEW.aircraft_designation_id
        JOIN aircraft_factory_packages package
          ON package.id = applicability.aircraft_factory_package_id
        WHERE listing.id = NEW.aircraft_sale_listing_id
          AND package.package_kind = 'trim_tier'
          AND (applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = NEW.aircraft_generation_id)
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= listing.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= listing.model_year)
      ))
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft assignment generation/package is not applicable to the designation and model year');
END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_identity_assignments
BEGIN SELECT RAISE(ABORT, 'listing aircraft identity assignment versions are immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_identity_assignment_immutable_delete
BEFORE DELETE ON aircraft_sale_listing_identity_assignments
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = OLD.aircraft_sale_listing_id
)
BEGIN SELECT RAISE(ABORT, 'listing aircraft identity assignment versions are immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_validate_insert
BEFORE INSERT ON aircraft_sale_listing_current_identity_assignments
WHEN NOT EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    AND assignment.supersedes_assignment_id IS NULL
)
BEGIN SELECT RAISE(ABORT, 'initial current aircraft identity must select the listing root assignment'); END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_validate_update
BEFORE UPDATE ON aircraft_sale_listing_current_identity_assignments
WHEN NEW.aircraft_sale_listing_id <> OLD.aircraft_sale_listing_id
  OR NEW.selected_at <= OLD.selected_at
  OR NOT EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    AND assignment.supersedes_assignment_id = OLD.identity_assignment_id
)
BEGIN SELECT RAISE(ABORT, 'current aircraft identity may advance only to its direct immutable successor'); END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_immutable_delete
BEFORE DELETE ON aircraft_sale_listing_current_identity_assignments
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listings listing
  WHERE listing.id = OLD.aircraft_sale_listing_id
)
BEGIN SELECT RAISE(ABORT, 'current aircraft identity may be deleted only with its parent listing'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_make_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft makes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_make_immutable_delete
BEFORE DELETE ON aircraft_makes
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_make_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft makes are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_model_family_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft model families are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_family_immutable_delete
BEFORE DELETE ON aircraft_model_families
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_model_family_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft model families are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_designation_immutable_update
BEFORE UPDATE ON aircraft_designations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_designation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft designations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_designation_immutable_delete
BEFORE DELETE ON aircraft_designations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_designation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft designations are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_generation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft generations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_generation_immutable_delete
BEFORE DELETE ON aircraft_generations
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_generation_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft generations are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_aircraft_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_factory_package_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft factory packages are immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_aircraft_package_immutable_delete
BEFORE DELETE ON aircraft_factory_packages
WHEN EXISTS (SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
             WHERE assignment.aircraft_factory_package_id = OLD.id)
BEGIN SELECT RAISE(ABORT, 'assigned aircraft factory packages are immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_generation_designation_immutable_update
BEFORE UPDATE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_generation_id = OLD.aircraft_generation_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'assigned generation/designation applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_generation_dimension_requires_resolution
BEFORE INSERT ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = current_assignment.aircraft_sale_listing_id
  JOIN aircraft_sale_listings listing
    ON listing.id = current_assignment.aircraft_sale_listing_id
  WHERE listing.ingestion_state = 'ready'
    AND assignment.aircraft_designation_id = NEW.aircraft_designation_id
    AND assignment.aircraft_generation_id IS NULL
)
BEGIN SELECT RAISE(ABORT, 'adding a generation dimension requires resolving affected ready listing assignments first'); END;
CREATE TRIGGER IF NOT EXISTS assigned_generation_designation_immutable_delete
BEFORE DELETE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_generation_id = OLD.aircraft_generation_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'assigned generation/designation applicability is immutable'); END;

CREATE TRIGGER IF NOT EXISTS assigned_package_applicability_immutable_update
BEFORE UPDATE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_factory_package_id = OLD.aircraft_factory_package_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
    AND (OLD.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id = OLD.aircraft_generation_id)
)
BEGIN SELECT RAISE(ABORT, 'assigned package applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS assigned_trim_tier_dimension_requires_resolution
BEFORE INSERT ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1
  FROM aircraft_factory_packages package
  CROSS JOIN aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = current_assignment.aircraft_sale_listing_id
  JOIN aircraft_sale_listings listing
    ON listing.id = current_assignment.aircraft_sale_listing_id
  WHERE package.id = NEW.aircraft_factory_package_id
    AND package.package_kind = 'trim_tier'
    AND listing.ingestion_state = 'ready'
    AND assignment.aircraft_designation_id = NEW.aircraft_designation_id
    AND assignment.aircraft_factory_package_id IS NULL
    AND (NEW.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id = NEW.aircraft_generation_id)
    AND (NEW.valid_from_model_year IS NULL
      OR NEW.valid_from_model_year <= listing.model_year)
    AND (NEW.valid_to_model_year IS NULL
      OR NEW.valid_to_model_year >= listing.model_year)
)
BEGIN SELECT RAISE(ABORT, 'adding a trim-tier dimension requires resolving affected ready listing assignments first'); END;
CREATE TRIGGER IF NOT EXISTS assigned_package_applicability_immutable_delete
BEFORE DELETE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_sale_listing_identity_assignments assignment
  WHERE assignment.aircraft_factory_package_id = OLD.aircraft_factory_package_id
    AND assignment.aircraft_designation_id = OLD.aircraft_designation_id
    AND (OLD.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id = OLD.aircraft_generation_id)
)
BEGIN SELECT RAISE(ABORT, 'assigned package applicability is immutable'); END;

-- Existing published rows predate this trust boundary. They cannot remain
-- grandfathered without a current evidence-backed assignment.
UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    ingestion_error = 'canonical aircraft identity migration: ready listing has no current FAA-backed curated assignment',
    ingestion_completed_at = NULL,
    is_verified = 0,
    updated_at = CURRENT_TIMESTAMP
WHERE ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_current_identity_assignments current_assignment
    WHERE current_assignment.aircraft_sale_listing_id = aircraft_sale_listings.id
  );

CREATE TRIGGER IF NOT EXISTS listing_ready_requires_canonical_aircraft_insert
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
BEGIN SELECT RAISE(ABORT, 'listing cannot be inserted ready before canonical aircraft assignment'); END;

CREATE TRIGGER IF NOT EXISTS listing_ready_requires_canonical_aircraft_update
BEFORE UPDATE OF ingestion_state, aircraft_model_variant_id, model_year, registration_number, serial_number
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready' AND NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.id
  JOIN aircraft_makes canonical_make ON canonical_make.id = assignment.aircraft_make_id
  JOIN aircraft_designations canonical_designation
    ON canonical_designation.id = assignment.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = canonical_designation.id
  JOIN faa_registry_snapshots snapshot ON snapshot.id = assignment.faa_registry_snapshot_id
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = snapshot.id
   AND aircraft.n_number = assignment.faa_n_number
   AND aircraft.source_record_sha256 = assignment.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id = assignment.aircraft_designation_id
  WHERE current_assignment.aircraft_sale_listing_id = NEW.id
    AND EXISTS (
      SELECT 1
      FROM faa_registry_snapshots latest_release
      WHERE latest_release.id = (
        SELECT id FROM faa_registry_snapshots
        ORDER BY snapshot_date DESC, id DESC LIMIT 1
      )
        AND latest_release.snapshot_date = snapshot.snapshot_date
        AND latest_release.archive_sha256 = snapshot.archive_sha256
    )
    AND upper(replace(replace(trim(NEW.registration_number), '-', ''), ' ', '')) = assignment.faa_n_number
    AND (NEW.serial_number IS NULL OR trim(NEW.serial_number) = ''
      OR aircraft.manufacturer_serial_raw IS NULL
      OR upper(replace(replace(trim(NEW.serial_number), '-', ''), ' ', ''))
        = upper(replace(replace(trim(aircraft.manufacturer_serial_raw), '-', ''), ' ', '')))
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(canonical_make.name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = canonical_make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
            = lower(replace(replace(replace(replace(replace(replace(replace(replace(replace(replace(trim(reference.manufacturer_name), ' ', ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
          AND (alias.aircraft_market_id IS NULL OR market.code IN ('GLOBAL', 'US'))
          AND (alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= NEW.model_year)
          AND (alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= NEW.model_year)
      )
    )
    AND (
      (assignment.aircraft_generation_id IS NULL AND NOT EXISTS (
        SELECT 1 FROM aircraft_generation_designations generation_link
        WHERE generation_link.aircraft_designation_id = assignment.aircraft_designation_id
      ))
      OR (assignment.aircraft_generation_id IS NOT NULL AND EXISTS (
        SELECT 1 FROM aircraft_generation_designations generation_link
        WHERE generation_link.aircraft_generation_id = assignment.aircraft_generation_id
          AND generation_link.aircraft_designation_id = assignment.aircraft_designation_id
      ))
    )
    AND (
      (assignment.aircraft_factory_package_id IS NULL AND NOT EXISTS (
        SELECT 1
        FROM aircraft_package_applicability applicability
        JOIN aircraft_factory_packages package
          ON package.id = applicability.aircraft_factory_package_id
        WHERE applicability.aircraft_designation_id = assignment.aircraft_designation_id
          AND package.package_kind = 'trim_tier'
          AND (applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = assignment.aircraft_generation_id)
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= NEW.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= NEW.model_year)
      ))
      OR (assignment.aircraft_factory_package_id IS NOT NULL AND EXISTS (
        SELECT 1
        FROM aircraft_package_applicability applicability
        WHERE applicability.aircraft_factory_package_id = assignment.aircraft_factory_package_id
          AND applicability.aircraft_designation_id = assignment.aircraft_designation_id
          AND (applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id = assignment.aircraft_generation_id)
          AND (applicability.valid_from_model_year IS NULL
            OR applicability.valid_from_model_year <= NEW.model_year)
          AND (applicability.valid_to_model_year IS NULL
            OR applicability.valid_to_model_year >= NEW.model_year)
      ))
    )
)
BEGIN SELECT RAISE(ABORT, 'ready listing requires a current canonical aircraft assignment matching current FAA identity'); END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint
) VALUES
  (
    '20260725_identity_deduplication_postconditions',
    6,
    'cd001240b48a1480fd8bbee39b9ddedbba01d00fad45cbac315cec7a243cf133'
  ),
  (
    '20260725_listing_aircraft_identity',
    2,
    '63fb5b5213fc9eb2b7b4dcb2b0be3a9f22a80d4acae49f64e68ec1302c1437be'
  )
ON CONFLICT (migration_name) DO NOTHING;


-- FAA-backed aircraft valuation compatibility projection.
-- Every unresolved new listing points at one schema-owned placeholder. Literal
-- extracted labels live only in aircraft_identity_observations.
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_pending_compatibility_placeholder (
  singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
  aircraft_manufacturer_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_manufacturers(id) ON DELETE RESTRICT,
  aircraft_model_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_models(id) ON DELETE RESTRICT,
  aircraft_model_variant_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_model_variants(id) ON DELETE RESTRICT
);

-- Parsed/manual fields are useful retrieval hints but are not quoted source
-- evidence. Keep them in an explicitly non-authoritative staging table rather
-- than weakening aircraft_identity_observations.exact_source_evidence.
CREATE TABLE IF NOT EXISTS aircraft_listing_identity_input_observations (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE SET NULL,
  source_url TEXT,
  observed_make TEXT NOT NULL,
  observed_family TEXT NOT NULL,
  observed_designation TEXT NOT NULL,
  model_year INTEGER NOT NULL CHECK (model_year BETWEEN 1900 AND 2200),
  serial_number TEXT,
  registration_number TEXT,
  input_json TEXT NOT NULL,
  observation_sha256 TEXT NOT NULL UNIQUE,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(observed_make)) > 0),
  CHECK (length(trim(observed_family)) > 0),
  CHECK (length(trim(observed_designation)) > 0),
  CHECK (length(trim(input_json)) > 0)
);
CREATE INDEX IF NOT EXISTS idx_aircraft_listing_identity_input_listing
  ON aircraft_listing_identity_input_observations (aircraft_sale_listing_id);

-- Raw input history is append-only. Deleting its parent listing preserves the
-- observation and may clear only the nullable listing reference through the
-- declared ON DELETE SET NULL action.
CREATE TRIGGER IF NOT EXISTS aircraft_listing_identity_input_append_only_update
BEFORE UPDATE ON aircraft_listing_identity_input_observations
WHEN NOT (
  OLD.aircraft_sale_listing_id IS NOT NULL
  AND NEW.aircraft_sale_listing_id IS NULL
  AND NOT EXISTS (
    SELECT 1 FROM aircraft_sale_listings listing
    WHERE listing.id = OLD.aircraft_sale_listing_id
  )
  AND NEW.id IS OLD.id
  AND NEW.source_url IS OLD.source_url
  AND NEW.observed_make IS OLD.observed_make
  AND NEW.observed_family IS OLD.observed_family
  AND NEW.observed_designation IS OLD.observed_designation
  AND NEW.model_year IS OLD.model_year
  AND NEW.serial_number IS OLD.serial_number
  AND NEW.registration_number IS OLD.registration_number
  AND NEW.input_json IS OLD.input_json
  AND NEW.observation_sha256 IS OLD.observation_sha256
  AND NEW.created_at IS OLD.created_at
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft listing identity input observations are append-only');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_listing_identity_input_append_only_delete
BEFORE DELETE ON aircraft_listing_identity_input_observations
BEGIN
  SELECT RAISE(ABORT, 'aircraft listing identity input observations are append-only');
END;

INSERT INTO aircraft_manufacturers (id, name, normalized_name)
VALUES (-1, 'Pending FAA curation', '__aircost_pending_faa_make__')
ON CONFLICT (normalized_name) DO NOTHING;

INSERT INTO aircraft_models (
  id, aircraft_manufacturer_id, name, normalized_name
)
SELECT -1, id, 'Pending FAA curation', '__aircost_pending_faa_family__'
FROM aircraft_manufacturers
WHERE normalized_name = '__aircost_pending_faa_make__'
ON CONFLICT (aircraft_manufacturer_id, normalized_name) DO NOTHING;

INSERT INTO aircraft_model_variants (
  id, aircraft_model_id, name, normalized_name
)
SELECT -1, id, 'Pending FAA curation', '__aircost_pending_faa_identity__'
FROM aircraft_models
WHERE normalized_name = '__aircost_pending_faa_family__'
  AND aircraft_manufacturer_id = (
    SELECT id FROM aircraft_manufacturers
    WHERE normalized_name = '__aircost_pending_faa_make__'
  )
ON CONFLICT (aircraft_model_id, normalized_name) DO NOTHING;

INSERT INTO aircraft_sale_listing_pending_compatibility_placeholder (
  singleton_id, aircraft_manufacturer_id, aircraft_model_id,
  aircraft_model_variant_id
)
SELECT 1, manufacturer.id, model.id, variant.id
FROM aircraft_manufacturers manufacturer
JOIN aircraft_models model
  ON model.aircraft_manufacturer_id = manufacturer.id
JOIN aircraft_model_variants variant
  ON variant.aircraft_model_id = model.id
WHERE manufacturer.name = 'Pending FAA curation'
  AND manufacturer.normalized_name = '__aircost_pending_faa_make__'
  AND model.name = 'Pending FAA curation'
  AND model.normalized_name = '__aircost_pending_faa_family__'
  AND variant.name = 'Pending FAA curation'
  AND variant.normalized_name = '__aircost_pending_faa_identity__'
ON CONFLICT (singleton_id) DO NOTHING;
-- The sole bridge from canonical aircraft identity to the legacy valuation
-- hierarchy. Provenance is copied from the live immutable assignment at
-- creation so the projection survives later deletion of the source listing.
CREATE TABLE IF NOT EXISTS aircraft_valuation_compatibility_projections (
  aircraft_model_variant_id INTEGER PRIMARY KEY
    REFERENCES aircraft_model_variants(id) ON DELETE RESTRICT,
  aircraft_make_id INTEGER NOT NULL,
  aircraft_model_family_id INTEGER NOT NULL,
  aircraft_designation_id INTEGER NOT NULL,
  aircraft_generation_id INTEGER,
  aircraft_factory_package_id INTEGER,
  created_from_aircraft_sale_listing_id INTEGER NOT NULL,
  created_from_identity_assignment_id INTEGER NOT NULL,
  identity_decision_id INTEGER NOT NULL
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  faa_registry_snapshot_id INTEGER NOT NULL
    REFERENCES faa_registry_snapshots(id) ON DELETE RESTRICT,
  faa_n_number TEXT NOT NULL,
  faa_source_record_sha256 TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (aircraft_model_family_id, aircraft_make_id)
    REFERENCES aircraft_model_families(id, aircraft_make_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_designation_id, aircraft_model_family_id)
    REFERENCES aircraft_designations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_generation_id, aircraft_model_family_id)
    REFERENCES aircraft_generations(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (aircraft_factory_package_id, aircraft_model_family_id)
    REFERENCES aircraft_factory_packages(id, aircraft_model_family_id) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_n_number)
    REFERENCES faa_registry_aircraft(snapshot_id, n_number) ON DELETE RESTRICT,
  FOREIGN KEY (faa_registry_snapshot_id, faa_source_record_sha256)
    REFERENCES faa_registry_aircraft(snapshot_id, source_record_sha256) ON DELETE RESTRICT,
  CHECK (aircraft_make_id > 0),
  CHECK (aircraft_model_family_id > 0),
  CHECK (aircraft_designation_id > 0),
  CHECK (aircraft_generation_id IS NULL OR aircraft_generation_id > 0),
  CHECK (aircraft_factory_package_id IS NULL OR aircraft_factory_package_id > 0),
  CHECK (created_from_aircraft_sale_listing_id > 0),
  CHECK (created_from_identity_assignment_id > 0)
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_aircraft_valuation_projection_identity
  ON aircraft_valuation_compatibility_projections (
    aircraft_make_id, aircraft_model_family_id, aircraft_designation_id,
    coalesce(aircraft_generation_id, 0),
    coalesce(aircraft_factory_package_id, 0)
  );

CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_pending_compatibility_placeholder
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility placeholder is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_immutable_delete
BEFORE DELETE ON aircraft_sale_listing_pending_compatibility_placeholder
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility placeholder is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_manufacturer_immutable_update
BEFORE UPDATE ON aircraft_manufacturers
WHEN OLD.id = (
  SELECT aircraft_manufacturer_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility manufacturer is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_manufacturer_immutable_delete
BEFORE DELETE ON aircraft_manufacturers
WHEN OLD.id = (
  SELECT aircraft_manufacturer_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility manufacturer is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_model_immutable_update
BEFORE UPDATE ON aircraft_models
WHEN OLD.id = (
  SELECT aircraft_model_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility model is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_model_immutable_delete
BEFORE DELETE ON aircraft_models
WHEN OLD.id = (
  SELECT aircraft_model_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility model is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_variant_immutable_update
BEFORE UPDATE ON aircraft_model_variants
WHEN OLD.id = (
  SELECT aircraft_model_variant_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility variant is immutable'); END;
CREATE TRIGGER IF NOT EXISTS pending_aircraft_placeholder_variant_immutable_delete
BEFORE DELETE ON aircraft_model_variants
WHEN OLD.id = (
  SELECT aircraft_model_variant_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
BEGIN SELECT RAISE(ABORT, 'pending aircraft compatibility variant is immutable'); END;

CREATE TRIGGER IF NOT EXISTS listing_insert_requires_aircraft_projection_or_placeholder
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.aircraft_model_variant_id <> (
  SELECT aircraft_model_variant_id
  FROM aircraft_sale_listing_pending_compatibility_placeholder
  WHERE singleton_id = 1
)
AND NOT EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_variant_id =
        NEW.aircraft_model_variant_id
)
BEGIN
  SELECT RAISE(ABORT, 'new listing must use the pending aircraft placeholder or an existing canonical projection');
END;

-- A transition is deliberately short-lived. Its insertion proves that the
-- target is either the exact existing projection or a fresh, unreferenced,
-- deterministic reserved-key variant for the assignment.
CREATE TABLE IF NOT EXISTS aircraft_valuation_projection_transitions (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  aircraft_sale_listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
  identity_assignment_id INTEGER NOT NULL,
  transition_kind TEXT NOT NULL CHECK (
    transition_kind IN ('initial', 'current_repair', 'successor')
  ),
  selected_at TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (identity_assignment_id, aircraft_sale_listing_id)
    REFERENCES aircraft_sale_listing_identity_assignments(
      id, aircraft_sale_listing_id
    ) ON DELETE CASCADE
);

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_validate_insert
BEFORE INSERT ON aircraft_valuation_projection_transitions
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = NEW.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = listing.id
  JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
  JOIN aircraft_model_families family
    ON family.id = assignment.aircraft_model_family_id
   AND family.aircraft_make_id = make.id
  JOIN aircraft_designations designation
    ON designation.id = assignment.aircraft_designation_id
   AND designation.aircraft_model_family_id = family.id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = assignment.faa_registry_snapshot_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND listing.ingestion_state <> 'ready'
    AND assignment.aircraft_make_id > 0
    AND assignment.aircraft_model_family_id > 0
    AND assignment.aircraft_designation_id > 0
    AND (
      assignment.aircraft_generation_id IS NULL
      OR assignment.aircraft_generation_id > 0
    )
    AND (
      assignment.aircraft_factory_package_id IS NULL
      OR assignment.aircraft_factory_package_id > 0
    )
    AND snapshot.id = (
      SELECT id FROM faa_registry_snapshots
      ORDER BY snapshot_date DESC, id DESC LIMIT 1
    )
    AND (
      (NEW.transition_kind = 'initial'
        AND assignment.supersedes_assignment_id IS NULL
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_current_identity_assignments current_assignment
          WHERE current_assignment.aircraft_sale_listing_id = listing.id
        ))
      OR (NEW.transition_kind = 'current_repair'
        AND EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_current_identity_assignments current_assignment
          WHERE current_assignment.aircraft_sale_listing_id = listing.id
            AND current_assignment.identity_assignment_id = assignment.id
        ))
      OR (NEW.transition_kind = 'successor'
        AND EXISTS (
          SELECT 1
          FROM aircraft_sale_listing_current_identity_assignments current_assignment
          WHERE current_assignment.aircraft_sale_listing_id = listing.id
            AND current_assignment.identity_assignment_id =
                  assignment.supersedes_assignment_id
        ))
    )
    AND (
      EXISTS (
        SELECT 1
        FROM aircraft_valuation_compatibility_projections projection
        WHERE projection.aircraft_make_id = assignment.aircraft_make_id
          AND projection.aircraft_model_family_id =
                assignment.aircraft_model_family_id
          AND projection.aircraft_designation_id =
                assignment.aircraft_designation_id
          AND projection.aircraft_generation_id IS
                assignment.aircraft_generation_id
          AND projection.aircraft_factory_package_id IS
                assignment.aircraft_factory_package_id
      )
      OR (
        (
          NOT EXISTS (
            SELECT 1 FROM aircraft_manufacturers
            WHERE normalized_name =
              '__aircost_projection_make_' || make.id || '__'
          )
          OR EXISTS (
            SELECT 1
            FROM aircraft_valuation_compatibility_projections projection
            JOIN aircraft_model_variants projected_variant
              ON projected_variant.id = projection.aircraft_model_variant_id
            JOIN aircraft_models projected_model
              ON projected_model.id = projected_variant.aircraft_model_id
            JOIN aircraft_manufacturers projected_manufacturer
              ON projected_manufacturer.id =
                   projected_model.aircraft_manufacturer_id
            WHERE projection.aircraft_make_id = make.id
              AND projected_manufacturer.name = make.name
              AND projected_manufacturer.normalized_name =
                   '__aircost_projection_make_' || make.id || '__'
          )
        )
        AND (
          NOT EXISTS (
            SELECT 1 FROM aircraft_models
            WHERE normalized_name =
              '__aircost_projection_family_' || family.id || '__'
          )
          OR EXISTS (
            SELECT 1
            FROM aircraft_valuation_compatibility_projections projection
            JOIN aircraft_model_variants projected_variant
              ON projected_variant.id = projection.aircraft_model_variant_id
            JOIN aircraft_models projected_model
              ON projected_model.id = projected_variant.aircraft_model_id
            WHERE projection.aircraft_model_family_id = family.id
              AND projected_model.name = family.name
              AND projected_model.normalized_name =
                   '__aircost_projection_family_' || family.id || '__'
          )
        )
        AND NOT EXISTS (
          SELECT 1 FROM aircraft_model_variants
          WHERE normalized_name =
            '__aircost_projection_identity_'
            || designation.id || '_'
            || coalesce(assignment.aircraft_generation_id, 0) || '_'
            || coalesce(assignment.aircraft_factory_package_id, 0) || '__'
        )
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft projection command requires a current FAA assignment and either an exact projection or collision-free reserved keys');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_immutable_update
BEFORE UPDATE ON aircraft_valuation_projection_transitions
BEGIN SELECT RAISE(ABORT, 'aircraft projection transitions are immutable'); END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_projection_validate_insert
BEFORE INSERT ON aircraft_valuation_compatibility_projections
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_valuation_projection_transitions transition
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = transition.identity_assignment_id
   AND assignment.aircraft_sale_listing_id =
         transition.aircraft_sale_listing_id
  JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
  JOIN aircraft_model_families family
    ON family.id = assignment.aircraft_model_family_id
   AND family.aircraft_make_id = make.id
  JOIN aircraft_designations designation
    ON designation.id = assignment.aircraft_designation_id
   AND designation.aircraft_model_family_id = family.id
  LEFT JOIN aircraft_generations generation
    ON generation.id = assignment.aircraft_generation_id
   AND generation.aircraft_model_family_id = family.id
  LEFT JOIN aircraft_factory_packages package
    ON package.id = assignment.aircraft_factory_package_id
   AND package.aircraft_model_family_id = family.id
  JOIN aircraft_model_variants legacy_variant
    ON legacy_variant.id = NEW.aircraft_model_variant_id
  JOIN aircraft_models legacy_model
    ON legacy_model.id = legacy_variant.aircraft_model_id
  JOIN aircraft_manufacturers legacy_manufacturer
    ON legacy_manufacturer.id = legacy_model.aircraft_manufacturer_id
  WHERE assignment.aircraft_make_id = NEW.aircraft_make_id
    AND assignment.aircraft_model_family_id = NEW.aircraft_model_family_id
    AND assignment.aircraft_designation_id = NEW.aircraft_designation_id
    AND assignment.aircraft_generation_id IS NEW.aircraft_generation_id
    AND assignment.aircraft_factory_package_id IS
          NEW.aircraft_factory_package_id
    AND assignment.aircraft_sale_listing_id =
          NEW.created_from_aircraft_sale_listing_id
    AND assignment.id = NEW.created_from_identity_assignment_id
    AND assignment.identity_decision_id = NEW.identity_decision_id
    AND assignment.identity_evidence_claim_id = NEW.identity_evidence_claim_id
    AND assignment.faa_registry_snapshot_id = NEW.faa_registry_snapshot_id
    AND assignment.faa_n_number = NEW.faa_n_number
    AND assignment.faa_source_record_sha256 = NEW.faa_source_record_sha256
    AND legacy_manufacturer.name = make.name
    AND legacy_manufacturer.normalized_name =
          '__aircost_projection_make_' || make.id || '__'
    AND legacy_model.name = family.name
    AND legacy_model.normalized_name =
          '__aircost_projection_family_' || family.id || '__'
    AND legacy_variant.name =
      designation.official_designation
      || CASE WHEN generation.id IS NULL THEN '' ELSE ' / ' || generation.name END
      || CASE WHEN package.id IS NULL THEN '' ELSE ' / ' || package.name END
    AND legacy_variant.normalized_name =
      '__aircost_projection_identity_'
      || designation.id || '_'
      || coalesce(generation.id, 0) || '_'
      || coalesce(package.id, 0) || '__'
    AND (
      assignment.aircraft_generation_id IS NULL
      OR EXISTS (
        SELECT 1 FROM aircraft_generation_designations applicability
        WHERE applicability.aircraft_generation_id =
              assignment.aircraft_generation_id
          AND applicability.aircraft_designation_id =
              assignment.aircraft_designation_id
      )
    )
    AND (
      assignment.aircraft_factory_package_id IS NULL
      OR EXISTS (
        SELECT 1 FROM aircraft_package_applicability applicability
        WHERE applicability.aircraft_factory_package_id =
              assignment.aircraft_factory_package_id
          AND applicability.aircraft_designation_id =
              assignment.aircraft_designation_id
          AND (
            applicability.aircraft_generation_id IS NULL
            OR applicability.aircraft_generation_id IS
                  assignment.aircraft_generation_id
          )
      )
    )
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_sale_listings child
      WHERE child.aircraft_model_variant_id = legacy_variant.id
    )
    AND NOT EXISTS (
      SELECT 1 FROM rental_aircraft_offerings child
      WHERE child.aircraft_model_variant_id = legacy_variant.id
    )
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft compatibility projection requires the active command, exact copied assignment provenance, and its fresh reserved hierarchy');
END;

-- A transition row is a command, not durable state. The command creates the
-- projection when needed, repoints the listing, selects the assignment, and
-- deletes itself inside the same INSERT statement. Any failed sub-step rolls
-- the entire statement back, so no committed bypass capability can remain.
CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_execute
AFTER INSERT ON aircraft_valuation_projection_transitions
BEGIN
  INSERT INTO aircraft_manufacturers (name, normalized_name)
  SELECT
    make.name,
    '__aircost_projection_make_' || make.id || '__'
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_makes make ON make.id = assignment.aircraft_make_id
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_manufacturers existing
      WHERE existing.normalized_name =
        '__aircost_projection_make_' || make.id || '__'
    );

  INSERT INTO aircraft_models (
    aircraft_manufacturer_id, name, normalized_name
  )
  SELECT
    legacy_manufacturer.id,
    family.name,
    '__aircost_projection_family_' || family.id || '__'
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_model_families family
    ON family.id = assignment.aircraft_model_family_id
  JOIN aircraft_manufacturers legacy_manufacturer
    ON legacy_manufacturer.normalized_name =
       '__aircost_projection_make_' || assignment.aircraft_make_id || '__'
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_models existing
      WHERE existing.normalized_name =
        '__aircost_projection_family_' || family.id || '__'
    );

  INSERT INTO aircraft_model_variants (
    aircraft_model_id, name, normalized_name
  )
  SELECT
    legacy_model.id,
    designation.official_designation
      || CASE WHEN generation.id IS NULL THEN '' ELSE ' / ' || generation.name END
      || CASE WHEN package.id IS NULL THEN '' ELSE ' / ' || package.name END,
    '__aircost_projection_identity_'
      || designation.id || '_'
      || coalesce(generation.id, 0) || '_'
      || coalesce(package.id, 0) || '__'
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_designations designation
    ON designation.id = assignment.aircraft_designation_id
  LEFT JOIN aircraft_generations generation
    ON generation.id = assignment.aircraft_generation_id
  LEFT JOIN aircraft_factory_packages package
    ON package.id = assignment.aircraft_factory_package_id
  JOIN aircraft_models legacy_model
    ON legacy_model.normalized_name =
       '__aircost_projection_family_'
       || assignment.aircraft_model_family_id || '__'
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1 FROM aircraft_model_variants existing
      WHERE existing.normalized_name =
        '__aircost_projection_identity_'
        || designation.id || '_'
        || coalesce(generation.id, 0) || '_'
        || coalesce(package.id, 0) || '__'
    );

  INSERT INTO aircraft_valuation_compatibility_projections (
    aircraft_model_variant_id,
    aircraft_make_id,
    aircraft_model_family_id,
    aircraft_designation_id,
    aircraft_generation_id,
    aircraft_factory_package_id,
    created_from_aircraft_sale_listing_id,
    created_from_identity_assignment_id,
    identity_decision_id,
    identity_evidence_claim_id,
    faa_registry_snapshot_id,
    faa_n_number,
    faa_source_record_sha256
  )
  SELECT
    legacy_variant.id,
    assignment.aircraft_make_id,
    assignment.aircraft_model_family_id,
    assignment.aircraft_designation_id,
    assignment.aircraft_generation_id,
    assignment.aircraft_factory_package_id,
    assignment.aircraft_sale_listing_id,
    assignment.id,
    assignment.identity_decision_id,
    assignment.identity_evidence_claim_id,
    assignment.faa_registry_snapshot_id,
    assignment.faa_n_number,
    assignment.faa_source_record_sha256
  FROM aircraft_sale_listing_identity_assignments assignment
  JOIN aircraft_model_variants legacy_variant
    ON legacy_variant.normalized_name =
       '__aircost_projection_identity_'
       || assignment.aircraft_designation_id || '_'
       || coalesce(assignment.aircraft_generation_id, 0) || '_'
       || coalesce(assignment.aircraft_factory_package_id, 0) || '__'
  WHERE assignment.id = NEW.identity_assignment_id
    AND assignment.aircraft_sale_listing_id =
          NEW.aircraft_sale_listing_id
    AND NOT EXISTS (
      SELECT 1
      FROM aircraft_valuation_compatibility_projections projection
      WHERE projection.aircraft_make_id = assignment.aircraft_make_id
        AND projection.aircraft_model_family_id =
              assignment.aircraft_model_family_id
        AND projection.aircraft_designation_id =
              assignment.aircraft_designation_id
        AND projection.aircraft_generation_id IS
              assignment.aircraft_generation_id
        AND projection.aircraft_factory_package_id IS
              assignment.aircraft_factory_package_id
    );

  UPDATE aircraft_sale_listings
  SET aircraft_model_variant_id = (
        SELECT projection.aircraft_model_variant_id
        FROM aircraft_valuation_compatibility_projections projection
        JOIN aircraft_sale_listing_identity_assignments assignment
          ON assignment.id = NEW.identity_assignment_id
         AND assignment.aircraft_sale_listing_id =
               NEW.aircraft_sale_listing_id
         AND projection.aircraft_make_id = assignment.aircraft_make_id
         AND projection.aircraft_model_family_id =
               assignment.aircraft_model_family_id
         AND projection.aircraft_designation_id =
               assignment.aircraft_designation_id
         AND projection.aircraft_generation_id IS
               assignment.aircraft_generation_id
         AND projection.aircraft_factory_package_id IS
               assignment.aircraft_factory_package_id
      ),
      updated_at = CURRENT_TIMESTAMP
  WHERE id = NEW.aircraft_sale_listing_id;

  INSERT INTO aircraft_sale_listing_current_identity_assignments (
    aircraft_sale_listing_id, identity_assignment_id, selected_at
  )
  SELECT
    NEW.aircraft_sale_listing_id, NEW.identity_assignment_id, NEW.selected_at
  WHERE NEW.transition_kind = 'initial';

  UPDATE aircraft_sale_listing_current_identity_assignments
  SET identity_assignment_id = NEW.identity_assignment_id,
      selected_at = NEW.selected_at
  WHERE NEW.transition_kind = 'successor'
    AND aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    AND identity_assignment_id = (
      SELECT supersedes_assignment_id
      FROM aircraft_sale_listing_identity_assignments
      WHERE id = NEW.identity_assignment_id
        AND aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
    );

  DELETE FROM aircraft_valuation_projection_transitions
  WHERE id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_projection_immutable_update
BEFORE UPDATE ON aircraft_valuation_compatibility_projections
BEGIN SELECT RAISE(ABORT, 'aircraft compatibility projections are immutable'); END;
CREATE TRIGGER IF NOT EXISTS aircraft_valuation_projection_immutable_delete
BEFORE DELETE ON aircraft_valuation_compatibility_projections
BEGIN SELECT RAISE(ABORT, 'aircraft compatibility projections are immutable'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_variant_immutable_update
BEFORE UPDATE ON aircraft_model_variants
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_variant_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft variants are immutable'); END;
CREATE TRIGGER IF NOT EXISTS projected_aircraft_variant_immutable_delete
BEFORE DELETE ON aircraft_model_variants
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_variant_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft variants are immutable'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_model_immutable_update
BEFORE UPDATE ON aircraft_models
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variants variant
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE variant.aircraft_model_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft models are immutable'); END;
CREATE TRIGGER IF NOT EXISTS projected_aircraft_model_immutable_delete
BEFORE DELETE ON aircraft_models
WHEN EXISTS (
  SELECT 1
  FROM aircraft_model_variants variant
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE variant.aircraft_model_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft models are immutable'); END;

CREATE TRIGGER IF NOT EXISTS projected_aircraft_manufacturer_immutable_update
BEFORE UPDATE ON aircraft_manufacturers
WHEN EXISTS (
  SELECT 1
  FROM aircraft_models model
  JOIN aircraft_model_variants variant ON variant.aircraft_model_id = model.id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE model.aircraft_manufacturer_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft manufacturers are immutable'); END;
CREATE TRIGGER IF NOT EXISTS projected_aircraft_manufacturer_immutable_delete
BEFORE DELETE ON aircraft_manufacturers
WHEN EXISTS (
  SELECT 1
  FROM aircraft_models model
  JOIN aircraft_model_variants variant ON variant.aircraft_model_id = model.id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id = variant.id
  WHERE model.aircraft_manufacturer_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'projected aircraft manufacturers are immutable'); END;

CREATE TRIGGER IF NOT EXISTS compatibility_projected_make_immutable_update
BEFORE UPDATE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft makes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_make_immutable_delete
BEFORE DELETE ON aircraft_makes
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_make_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft makes are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_family_immutable_update
BEFORE UPDATE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_family_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft families are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_family_immutable_delete
BEFORE DELETE ON aircraft_model_families
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_model_family_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft families are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_designation_immutable_update
BEFORE UPDATE ON aircraft_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_designation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft designations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_designation_immutable_delete
BEFORE DELETE ON aircraft_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_designation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected canonical aircraft designations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_immutable_update
BEFORE UPDATE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft generations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_immutable_delete
BEFORE DELETE ON aircraft_generations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft generations are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_immutable_update
BEFORE UPDATE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft packages are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_immutable_delete
BEFORE DELETE ON aircraft_factory_packages
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id = OLD.id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected aircraft packages are immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_link_immutable_update
BEFORE UPDATE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.aircraft_generation_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected generation applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_generation_link_immutable_delete
BEFORE DELETE ON aircraft_generation_designations
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_generation_id = OLD.aircraft_generation_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected generation applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_link_immutable_update
BEFORE UPDATE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id =
        OLD.aircraft_factory_package_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
    AND (
      OLD.aircraft_generation_id IS NULL
      OR projection.aircraft_generation_id = OLD.aircraft_generation_id
    )
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected package applicability is immutable'); END;
CREATE TRIGGER IF NOT EXISTS compatibility_projected_package_link_immutable_delete
BEFORE DELETE ON aircraft_package_applicability
WHEN EXISTS (
  SELECT 1 FROM aircraft_valuation_compatibility_projections projection
  WHERE projection.aircraft_factory_package_id =
        OLD.aircraft_factory_package_id
    AND projection.aircraft_designation_id = OLD.aircraft_designation_id
    AND (
      OLD.aircraft_generation_id IS NULL
      OR projection.aircraft_generation_id = OLD.aircraft_generation_id
    )
)
BEGIN SELECT RAISE(ABORT, 'compatibility-projected package applicability is immutable'); END;

-- An assigned listing can change its compatibility FK only while an exact
-- transition is active. Routine updates retain the existing projected FK.
CREATE TRIGGER IF NOT EXISTS listing_aircraft_projection_transition_update
BEFORE UPDATE OF aircraft_model_variant_id ON aircraft_sale_listings
WHEN NEW.aircraft_model_variant_id <> OLD.aircraft_model_variant_id
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_valuation_projection_transitions transition
    JOIN aircraft_sale_listing_identity_assignments assignment
      ON assignment.id = transition.identity_assignment_id
     AND assignment.aircraft_sale_listing_id =
           transition.aircraft_sale_listing_id
    JOIN aircraft_valuation_compatibility_projections projection
      ON projection.aircraft_make_id = assignment.aircraft_make_id
     AND projection.aircraft_model_family_id =
           assignment.aircraft_model_family_id
     AND projection.aircraft_designation_id =
           assignment.aircraft_designation_id
     AND projection.aircraft_generation_id IS
           assignment.aircraft_generation_id
     AND projection.aircraft_factory_package_id IS
           assignment.aircraft_factory_package_id
    WHERE transition.aircraft_sale_listing_id = NEW.id
      AND projection.aircraft_model_variant_id =
            NEW.aircraft_model_variant_id
  )
BEGIN
  SELECT RAISE(ABORT, 'listing aircraft compatibility FK may change only through an exact guarded transition');
END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_projection_insert
BEFORE INSERT ON aircraft_sale_listing_current_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_valuation_projection_transitions transition
    ON transition.aircraft_sale_listing_id = listing.id
   AND transition.identity_assignment_id = NEW.identity_assignment_id
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = NEW.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id =
         listing.aircraft_model_variant_id
   AND projection.aircraft_make_id = assignment.aircraft_make_id
   AND projection.aircraft_model_family_id =
         assignment.aircraft_model_family_id
   AND projection.aircraft_designation_id =
         assignment.aircraft_designation_id
   AND projection.aircraft_generation_id IS
         assignment.aircraft_generation_id
   AND projection.aircraft_factory_package_id IS
         assignment.aircraft_factory_package_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
)
BEGIN
  SELECT RAISE(ABORT, 'current aircraft identity requires the exact guarded listing projection');
END;

CREATE TRIGGER IF NOT EXISTS listing_current_identity_projection_update
BEFORE UPDATE OF identity_assignment_id
ON aircraft_sale_listing_current_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_valuation_projection_transitions transition
    ON transition.aircraft_sale_listing_id = listing.id
   AND transition.identity_assignment_id = NEW.identity_assignment_id
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = NEW.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.aircraft_sale_listing_id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id =
         listing.aircraft_model_variant_id
   AND projection.aircraft_make_id = assignment.aircraft_make_id
   AND projection.aircraft_model_family_id =
         assignment.aircraft_model_family_id
   AND projection.aircraft_designation_id =
         assignment.aircraft_designation_id
   AND projection.aircraft_generation_id IS
         assignment.aircraft_generation_id
   AND projection.aircraft_factory_package_id IS
         assignment.aircraft_factory_package_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
)
BEGIN
  SELECT RAISE(ABORT, 'current aircraft identity requires the exact guarded listing projection');
END;

CREATE TRIGGER IF NOT EXISTS aircraft_valuation_transition_validate_delete
BEFORE DELETE ON aircraft_valuation_projection_transitions
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN aircraft_sale_listing_current_identity_assignments current_assignment
    ON current_assignment.aircraft_sale_listing_id = listing.id
   AND current_assignment.identity_assignment_id = OLD.identity_assignment_id
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = listing.id
  JOIN aircraft_valuation_compatibility_projections projection
    ON projection.aircraft_model_variant_id =
         listing.aircraft_model_variant_id
   AND projection.aircraft_make_id = assignment.aircraft_make_id
   AND projection.aircraft_model_family_id =
         assignment.aircraft_model_family_id
   AND projection.aircraft_designation_id =
         assignment.aircraft_designation_id
   AND projection.aircraft_generation_id IS
         assignment.aircraft_generation_id
   AND projection.aircraft_factory_package_id IS
         assignment.aircraft_factory_package_id
  WHERE listing.id = OLD.aircraft_sale_listing_id
)
BEGIN
  SELECT RAISE(ABORT, 'aircraft projection transition cannot close before exact pointer and listing projection');
END;

CREATE VIEW IF NOT EXISTS aircraft_sale_listing_exact_compatibility_projections AS
SELECT
  listing.id AS listing_id,
  current_assignment.identity_assignment_id,
  listing.aircraft_model_variant_id,
  assignment.aircraft_make_id,
  assignment.aircraft_model_family_id,
  assignment.aircraft_designation_id,
  assignment.aircraft_generation_id,
  assignment.aircraft_factory_package_id
FROM aircraft_sale_listings listing
JOIN aircraft_sale_listing_current_identity_assignments current_assignment
  ON current_assignment.aircraft_sale_listing_id = listing.id
JOIN aircraft_sale_listing_identity_assignments assignment
  ON assignment.id = current_assignment.identity_assignment_id
 AND assignment.aircraft_sale_listing_id = listing.id
JOIN aircraft_valuation_compatibility_projections projection
  ON projection.aircraft_model_variant_id =
       listing.aircraft_model_variant_id
 AND projection.aircraft_make_id = assignment.aircraft_make_id
 AND projection.aircraft_model_family_id =
       assignment.aircraft_model_family_id
 AND projection.aircraft_designation_id =
       assignment.aircraft_designation_id
 AND projection.aircraft_generation_id IS assignment.aircraft_generation_id
 AND projection.aircraft_factory_package_id IS
       assignment.aircraft_factory_package_id;

UPDATE aircraft_sale_listings
SET ingestion_state = 'quarantined',
    ingestion_error =
      'aircraft compatibility projection migration: ready listing has no exact canonical projection',
    ingestion_completed_at = NULL,
    is_verified = 0,
    updated_at = CURRENT_TIMESTAMP
WHERE ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_exact_compatibility_projections exact_projection
    WHERE exact_projection.listing_id = aircraft_sale_listings.id
  );

CREATE TRIGGER IF NOT EXISTS listing_ready_requires_aircraft_projection
BEFORE UPDATE OF ingestion_state, aircraft_model_variant_id
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_current_identity_assignments current_assignment
    JOIN aircraft_sale_listing_identity_assignments assignment
      ON assignment.id = current_assignment.identity_assignment_id
     AND assignment.aircraft_sale_listing_id =
           current_assignment.aircraft_sale_listing_id
    JOIN aircraft_valuation_compatibility_projections projection
      ON projection.aircraft_model_variant_id =
           NEW.aircraft_model_variant_id
     AND projection.aircraft_make_id = assignment.aircraft_make_id
     AND projection.aircraft_model_family_id =
           assignment.aircraft_model_family_id
     AND projection.aircraft_designation_id =
           assignment.aircraft_designation_id
     AND projection.aircraft_generation_id IS
           assignment.aircraft_generation_id
     AND projection.aircraft_factory_package_id IS
           assignment.aircraft_factory_package_id
    WHERE current_assignment.aircraft_sale_listing_id = NEW.id
  )
BEGIN
  SELECT RAISE(ABORT, 'ready listing requires its exact canonical aircraft compatibility projection');
END;

CREATE TRIGGER IF NOT EXISTS listing_ready_insert_requires_aircraft_projection
BEFORE INSERT ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
  AND NOT EXISTS (
    SELECT 1
    FROM aircraft_sale_listing_exact_compatibility_projections exact_projection
    WHERE exact_projection.listing_id = NEW.id
      AND exact_projection.aircraft_model_variant_id =
            NEW.aircraft_model_variant_id
  )
BEGIN
  SELECT RAISE(ABORT, 'ready listing must first persist its exact canonical aircraft compatibility projection');
END;

CREATE TRIGGER IF NOT EXISTS listing_ready_rejects_pending_aircraft_placeholder
BEFORE UPDATE OF ingestion_state, aircraft_model_variant_id
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready'
  AND NEW.aircraft_model_variant_id = (
    SELECT aircraft_model_variant_id
    FROM aircraft_sale_listing_pending_compatibility_placeholder
    WHERE singleton_id = 1
  )
BEGIN
  SELECT RAISE(ABORT, 'pending aircraft compatibility placeholder cannot become ready');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260726_listing_aircraft_compatibility_projection',
  2,
  '0a182d5972d62be3d906395df8d08b741bc3e23d713badf7596b360048aa45ba',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260808_avionics_descriptive_consolidation',
  1,
  '3aacf958efa7fb5e24c5897cf0369d40cb506b2a22444d629ea0a76462ce1a70',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

-- Approved catalog truth is broader than no-grounding reuse eligibility.
-- This positive-only cache is populated only by the current curation policy;
-- existing approved products are deliberately not bootstrapped into it.
CREATE TABLE IF NOT EXISTS avionics_product_reuse_attestations (
  avionics_model_id INTEGER PRIMARY KEY
    REFERENCES avionics_models(id) ON DELETE CASCADE,
  avionics_authoritative_source_origin_id INTEGER NOT NULL
    REFERENCES avionics_authoritative_source_origins(id) ON DELETE RESTRICT,
  policy_version TEXT NOT NULL
    CHECK (policy_version = 'avionics_reuse_v2'),
  product_fingerprint TEXT NOT NULL,
  attested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(product_fingerprint) = 64),
  CHECK (product_fingerprint = lower(product_fingerprint)),
  CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*')
);

CREATE INDEX IF NOT EXISTS idx_avionics_product_reuse_origin
  ON avionics_product_reuse_attestations (
    avionics_authoritative_source_origin_id
  );

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_attestations_validate_insert
BEFORE INSERT ON avionics_product_reuse_attestations
WHEN NOT EXISTS (
  SELECT 1
  FROM avionics_models model
  JOIN avionics_approved_product_identities product_identity
    ON product_identity.avionics_model_id = model.id
  JOIN avionics_active_authoritative_source_origins source_origin
    ON source_origin.id =
       NEW.avionics_authoritative_source_origin_id
   AND source_origin.authority_kind = 'manufacturer_primary'
  JOIN avionics_manufacturer_effective_identities origin_identity
    ON origin_identity.identity_id =
       source_origin.avionics_manufacturer_identity_id
   AND origin_identity.avionics_manufacturer_identity_id =
       product_identity.avionics_manufacturer_identity_id
  WHERE model.id = NEW.avionics_model_id
    AND model.catalog_status = 'approved'
)
BEGIN
  SELECT RAISE(
    ABORT,
    'avionics reuse attestation requires an approved product bound to one active exact manufacturer origin'
  );
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_attestations_immutable_update
BEFORE UPDATE ON avionics_product_reuse_attestations
BEGIN
  SELECT RAISE(
    ABORT,
    'avionics reuse attestations are replaced, never updated'
  );
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_invalidate_type_insert
AFTER INSERT ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_invalidate_type_delete
AFTER DELETE ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = OLD.avionics_model_id;
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_invalidate_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id ON avionics_model_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id IN (
    OLD.avionics_model_id, NEW.avionics_model_id
  );
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_invalidate_capability_update
AFTER UPDATE OF name, normalized_name ON avionics_types
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id IN (
    SELECT membership.avionics_model_id
    FROM avionics_model_types membership
    WHERE membership.avionics_type_id = NEW.id
  );
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_invalidate_identity_update
AFTER UPDATE ON avionics_approved_product_identities
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER IF NOT EXISTS
  avionics_product_reuse_invalidate_origin_revocation
AFTER INSERT ON avionics_authoritative_source_origin_revocations
BEGIN
  DELETE FROM avionics_product_reuse_attestations
  WHERE avionics_authoritative_source_origin_id =
        NEW.avionics_authoritative_source_origin_id;
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260803_avionics_product_reuse_attestations',
  2,
  '8ad6e935e1222a03e2da4848a9e3c6f4b7f50ee027a6e50ede3b692d034cae55',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260807_avionics_product_reuse_v2',
  1,
  'efcec97dff7c11299536c46a602a4c0e680690434c4bdfb6ba7730b7305b87dc',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260804_avionics_grounded_evidence_refresh',
  1,
  '0c44e30c662d8f51c11f7db883251c1356cfda4d53957df038988c32d3b91399',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

-- One-use capability proving that one exact grounded identity resolution may
-- authorize one component of one capture-bound listing. It is consumed only
-- when the final listing link and same-case authorization commit atomically.
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_grounded_capabilities (
    listing_id INTEGER NOT NULL
      REFERENCES aircraft_sale_listings(id) ON DELETE CASCADE,
    plugin_submission_id INTEGER NOT NULL
      REFERENCES plugin_submissions(id) ON DELETE CASCADE,
    occurrence_index INTEGER NOT NULL CHECK (occurrence_index >= 0),
    occurrence_role TEXT NOT NULL
      CHECK (occurrence_role IN ('primary', 'replacement')),
    avionics_model_id INTEGER NOT NULL
      REFERENCES avionics_models(id) ON DELETE CASCADE,
    requested_quantity INTEGER NOT NULL CHECK (requested_quantity > 0),
    configuration_action TEXT NOT NULL
      CHECK (configuration_action IN ('installed', 'replaces', 'removes')),
    request_sha256 TEXT NOT NULL,
    capability_sha256 TEXT NOT NULL,
    grounded_resolution_sha256 TEXT NOT NULL,
    evidence_capture_sha256 TEXT NOT NULL,
    extracted_listing_sha256 TEXT NOT NULL,
    product_fingerprint TEXT NOT NULL,
    collision_closure_sha256 TEXT NOT NULL,
    policy_version TEXT NOT NULL
      CHECK (policy_version = 'listing_avionics_grounded_capability_v1'),
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (
      listing_id, plugin_submission_id, occurrence_index, occurrence_role
    ),
    CHECK (occurrence_role = 'primary' OR requested_quantity = 1),
    CHECK (
      occurrence_role = 'primary'
      OR configuration_action IN ('replaces', 'removes')
    ),
    CHECK (length(request_sha256) = 64),
    CHECK (request_sha256 = lower(request_sha256)),
    CHECK (request_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(capability_sha256) = 64),
    CHECK (capability_sha256 = lower(capability_sha256)),
    CHECK (capability_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(grounded_resolution_sha256) = 64),
    CHECK (grounded_resolution_sha256 = lower(grounded_resolution_sha256)),
    CHECK (grounded_resolution_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(evidence_capture_sha256) = 64),
    CHECK (evidence_capture_sha256 = lower(evidence_capture_sha256)),
    CHECK (evidence_capture_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(extracted_listing_sha256) = 64),
    CHECK (extracted_listing_sha256 = lower(extracted_listing_sha256)),
    CHECK (extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(product_fingerprint) = 64),
    CHECK (product_fingerprint = lower(product_fingerprint)),
    CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(collision_closure_sha256) = 64),
    CHECK (collision_closure_sha256 = lower(collision_closure_sha256)),
    CHECK (collision_closure_sha256 NOT GLOB '*[^0-9a-f]*')
  );

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_model
ON aircraft_sale_listing_avionics_grounded_capabilities (avionics_model_id);

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_grounded_capabilities_submission
ON aircraft_sale_listing_avionics_grounded_capabilities (plugin_submission_id);

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_grounded_capabilities_validate_insert
BEFORE INSERT ON aircraft_sale_listing_avionics_grounded_capabilities
WHEN NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.canonical_listing_id = NEW.listing_id
    AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
    AND submission.extracted_listing_json IS NOT NULL
    AND submission.extraction_error IS NULL
)
OR NOT EXISTS (
  SELECT 1
  FROM avionics_approved_product_graph_identities approved
  WHERE approved.avionics_model_id = NEW.avionics_model_id
)
BEGIN
  SELECT RAISE(ABORT, 'grounded avionics capability requires its exact current capture-bound listing and approved product');
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_grounded_capabilities_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_grounded_capabilities
BEGIN
  SELECT RAISE(ABORT, 'grounded avionics capabilities are immutable');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260825_listing_avionics_grounded_capabilities',
  1,
  'a7a249e910f4c16530760d18786f106f11f3b36a25c6a3e80fa8adacd1b79b31',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

-- Exact authorization for one listing-link component. Manufacturer-reuse
-- authorizations bind the current global attestation; same-case authorizations
-- bind the transient grounded resolution that approved this exact association.
CREATE TABLE IF NOT EXISTS aircraft_sale_listing_avionics_authorizations (
    listing_link_id INTEGER NOT NULL
      REFERENCES aircraft_sale_listing_avionics(id) ON DELETE CASCADE,
    association_role TEXT NOT NULL
      CHECK (association_role IN ('installed', 'replacement')),
    avionics_model_id INTEGER NOT NULL
      REFERENCES avionics_models(id) ON DELETE CASCADE,
    authorization_kind TEXT NOT NULL
      CHECK (authorization_kind IN ('manufacturer_reuse', 'same_case_grounded')),
    observation_sha256 TEXT NOT NULL,
    product_fingerprint TEXT NOT NULL,
    grounded_resolution_sha256 TEXT,
    evidence_capture_sha256 TEXT NOT NULL,
    plugin_submission_id INTEGER
      REFERENCES plugin_submissions(id) ON DELETE CASCADE,
    extracted_listing_sha256 TEXT,
    collision_closure_sha256 TEXT NOT NULL,
    policy_version TEXT NOT NULL
      CHECK (policy_version = 'listing_avionics_authorization_v2'),
    authorized_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (listing_link_id, association_role),
    CHECK (length(observation_sha256) = 64),
    CHECK (observation_sha256 = lower(observation_sha256)),
    CHECK (observation_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(product_fingerprint) = 64),
    CHECK (product_fingerprint = lower(product_fingerprint)),
    CHECK (product_fingerprint NOT GLOB '*[^0-9a-f]*'),
    CHECK (length(evidence_capture_sha256) = 64),
    CHECK (evidence_capture_sha256 = lower(evidence_capture_sha256)),
    CHECK (evidence_capture_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (extracted_listing_sha256 IS NULL OR (
      length(extracted_listing_sha256) = 64
      AND extracted_listing_sha256 = lower(extracted_listing_sha256)
      AND extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'
    )),
    CHECK (length(collision_closure_sha256) = 64),
    CHECK (collision_closure_sha256 = lower(collision_closure_sha256)),
    CHECK (collision_closure_sha256 NOT GLOB '*[^0-9a-f]*'),
    CHECK (
      (authorization_kind = 'manufacturer_reuse'
        AND grounded_resolution_sha256 IS NULL
        AND plugin_submission_id IS NULL
        AND extracted_listing_sha256 IS NULL)
      OR
      (authorization_kind = 'same_case_grounded'
        AND length(grounded_resolution_sha256) = 64
        AND grounded_resolution_sha256 = lower(grounded_resolution_sha256)
        AND grounded_resolution_sha256 NOT GLOB '*[^0-9a-f]*'
        AND plugin_submission_id IS NOT NULL
        AND extracted_listing_sha256 IS NOT NULL)
    )
  );

CREATE INDEX IF NOT EXISTS
  idx_listing_avionics_authorizations_model
ON aircraft_sale_listing_avionics_authorizations (avionics_model_id);

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_validate_insert
BEFORE INSERT ON aircraft_sale_listing_avionics_authorizations
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_avionics link
  WHERE link.id = NEW.listing_link_id
    AND link.source_confidence = 'high'
    AND length(trim(COALESCE(link.source_notes, ''))) > 0
    AND (
      (NEW.association_role = 'installed'
        AND link.avionics_model_id = NEW.avionics_model_id
      )
      OR
      (NEW.association_role = 'replacement'
        AND link.configuration_action IN ('replaces', 'removes')
        AND link.replaces_avionics_model_id = NEW.avionics_model_id
      )
    )
    AND (
      (NEW.authorization_kind = 'manufacturer_reuse'
        AND EXISTS (
          SELECT 1 FROM plugin_submissions capture
          WHERE capture.canonical_listing_id = link.aircraft_sale_listing_id
            AND capture.rendered_html_sha256 = NEW.evidence_capture_sha256
            AND instr(capture.rendered_html, link.source_notes) > 0
        )
        AND EXISTS (
          SELECT 1 FROM avionics_product_reuse_attestations attestation
          WHERE attestation.avionics_model_id = NEW.avionics_model_id
            AND attestation.product_fingerprint = NEW.product_fingerprint
        ))
      OR
      (NEW.authorization_kind = 'same_case_grounded'
        AND EXISTS (
          SELECT 1 FROM plugin_submissions submission
          WHERE submission.id = NEW.plugin_submission_id
            AND submission.canonical_listing_id = link.aircraft_sale_listing_id
            AND submission.rendered_html_sha256 = NEW.evidence_capture_sha256
            AND submission.extracted_listing_json IS NOT NULL
            AND submission.extraction_error IS NULL
            AND instr(submission.rendered_html, link.source_notes) > 0
        )
        AND EXISTS (
          SELECT 1 FROM avionics_approved_product_graph_identities identity
          WHERE identity.avionics_model_id = NEW.avionics_model_id
        ))
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'listing avionics authorization requires the exact current link role, retained capture, and product proof'
  );
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_immutable_update
BEFORE UPDATE ON aircraft_sale_listing_avionics_authorizations
BEGIN
  SELECT RAISE(
    ABORT,
    'listing avionics authorizations are replaced, never updated'
  );
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_link_update
AFTER UPDATE OF
  aircraft_sale_listing_id,
  avionics_model_id,
  quantity,
  source_notes,
  source_confidence,
  configuration_action,
  replaces_avionics_model_id
ON aircraft_sale_listing_avionics
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE listing_link_id = NEW.id;
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_reuse_delete
AFTER DELETE ON avionics_product_reuse_attestations
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'manufacturer_reuse'
    AND avionics_model_id = OLD.avionics_model_id;
END;


CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_model_proof_update
AFTER UPDATE OF
  avionics_manufacturer_id, name, normalized_name, catalog_status,
  manufacturer_identifier_kind, manufacturer_identifier,
  normalized_manufacturer_identifier, identity_source_url,
  identity_source_title, identity_evidence_text
ON avionics_models
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.id;
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_model_type_insert
AFTER INSERT ON avionics_model_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_model_type_delete
AFTER DELETE ON avionics_model_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.avionics_model_id;
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_model_type_update
AFTER UPDATE OF avionics_model_id, avionics_type_id ON avionics_model_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (OLD.avionics_model_id, NEW.avionics_model_id);
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_type_update
AFTER UPDATE OF name, normalized_name ON avionics_types
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT avionics_model_id FROM avionics_model_types
      WHERE avionics_type_id = OLD.id
    );
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_graph_insert
AFTER INSERT ON avionics_approved_product_identities
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = NEW.avionics_model_id;
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_graph_delete
AFTER DELETE ON avionics_approved_product_identities
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id = OLD.avionics_model_id;
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_graph_update
AFTER UPDATE OF
  avionics_model_id, avionics_manufacturer_identity_id,
  canonical_product_key, manufacturer_identifier_kind,
  canonical_identifier_key
ON avionics_approved_product_identities
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (OLD.avionics_model_id, NEW.avionics_model_id);
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_manufacturer_update
AFTER UPDATE OF name, normalized_name ON avionics_manufacturers
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT id FROM avionics_models
      WHERE avionics_manufacturer_id = OLD.id
    );
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_origin_revocation
AFTER INSERT ON avionics_authoritative_source_origin_revocations
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND avionics_model_id IN (
      SELECT model.id
      FROM avionics_models model
      JOIN avionics_approved_product_graph_identities product_identity
        ON product_identity.avionics_model_id = model.id
      JOIN avionics_authoritative_source_origins source_origin
        ON source_origin.id =
             NEW.avionics_authoritative_source_origin_id
      LEFT JOIN avionics_manufacturer_effective_identities origin_identity
        ON origin_identity.identity_id =
             source_origin.avionics_manufacturer_identity_id
      WHERE (
          lower(trim(model.identity_source_url)) = source_origin.https_origin
          OR substr(
              lower(trim(model.identity_source_url)),
              1,
              length(source_origin.https_origin) + 1
            ) IN (
              source_origin.https_origin || '/',
              source_origin.https_origin || '?',
              source_origin.https_origin || '#'
            )
        )
        AND (
          source_origin.authority_kind = 'regulator_primary'
          OR (
            source_origin.authority_kind = 'manufacturer_primary'
            AND origin_identity.avionics_manufacturer_identity_id =
                  product_identity.avionics_manufacturer_identity_id
          )
        )
    );
END;


CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_capture_delete
AFTER DELETE ON plugin_submissions
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE evidence_capture_sha256 = OLD.rendered_html_sha256
    AND EXISTS (
      SELECT 1 FROM aircraft_sale_listing_avionics link
      WHERE link.id =
              aircraft_sale_listing_avionics_authorizations.listing_link_id
        AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
        AND length(trim(COALESCE(link.source_notes, ''))) > 0
        AND instr(OLD.rendered_html, link.source_notes) > 0
        AND NOT EXISTS (
          SELECT 1 FROM plugin_submissions retained_capture
          WHERE retained_capture.canonical_listing_id =
                  link.aircraft_sale_listing_id
            AND retained_capture.rendered_html_sha256 =
                  aircraft_sale_listing_avionics_authorizations.evidence_capture_sha256
            AND instr(retained_capture.rendered_html, link.source_notes) > 0
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_capture_update
AFTER UPDATE OF canonical_listing_id, rendered_html, rendered_html_sha256
ON plugin_submissions
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE evidence_capture_sha256 = OLD.rendered_html_sha256
    AND EXISTS (
      SELECT 1 FROM aircraft_sale_listing_avionics link
      WHERE link.id =
              aircraft_sale_listing_avionics_authorizations.listing_link_id
        AND link.aircraft_sale_listing_id = OLD.canonical_listing_id
        AND length(trim(COALESCE(link.source_notes, ''))) > 0
        AND instr(OLD.rendered_html, link.source_notes) > 0
        AND NOT EXISTS (
          SELECT 1 FROM plugin_submissions retained_capture
          WHERE retained_capture.canonical_listing_id =
                  link.aircraft_sale_listing_id
            AND retained_capture.rendered_html_sha256 =
                  aircraft_sale_listing_avionics_authorizations.evidence_capture_sha256
            AND instr(retained_capture.rendered_html, link.source_notes) > 0
        )
    );
END;

CREATE TRIGGER IF NOT EXISTS
  listing_avionics_authorizations_invalidate_submission_checkpoint_update
AFTER UPDATE OF
  canonical_listing_id,
  rendered_html,
  rendered_html_sha256,
  extracted_listing_json,
  extraction_error
ON plugin_submissions
BEGIN
  DELETE FROM aircraft_sale_listing_avionics_authorizations
  WHERE authorization_kind = 'same_case_grounded'
    AND plugin_submission_id = OLD.id;
END;


INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260818_listing_avionics_association_authorizations',
  1,
  'bbb76c8535647f2ecaab3179d5ef483bdef9ca23a0e14e3fd0888912fc3d90f9',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260818_listing_avionics_authorization_hash_domain_reset',
  1,
  'cd0c1e10c508017f7053d0ab418e627ef993029ab7523a045eb7b66b802d5033',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name,
  contract_version,
  contract_fingerprint,
  installed_at
) VALUES (
  '20260731_avionics_human_reviewed_consolidation',
  1,
  '93a641a0f653eacf0c8413bdb697a35c588fe34efc1419d30bf65146c8b2d55a',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260801_avionics_authoritative_source_origins',
  2,
  'f78087f6354d93d78dc8cebc895f285e38a91ca6f72dc2351acaaa88b49f9620',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260728_aircraft_identity_no_supported_selection',
  2,
  '2c61547aae5158dd0a5393ca49218f0f3aada7d9b87caf950fa27fe2953d7dee',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

-- Canonical aircraft hierarchy retrieval keys are mechanical lookup keys, not
-- manufacturer aliases: lowercase ASCII alphanumerics with all other
-- characters treated as collapsed separators.
CREATE TRIGGER IF NOT EXISTS aircraft_make_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_makes
BEGIN
  SELECT RAISE(ABORT, 'aircraft make requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER IF NOT EXISTS aircraft_make_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_makes
BEGIN
  SELECT RAISE(ABORT, 'aircraft make requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER IF NOT EXISTS aircraft_family_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_model_families
BEGIN
  SELECT RAISE(ABORT, 'aircraft family requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;
CREATE TRIGGER IF NOT EXISTS aircraft_family_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_model_families
BEGIN
  SELECT RAISE(ABORT, 'aircraft family requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER IF NOT EXISTS aircraft_generation_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_generations
BEGIN
  SELECT RAISE(ABORT, 'aircraft generation requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;
CREATE TRIGGER IF NOT EXISTS aircraft_generation_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_generations
BEGIN
  SELECT RAISE(ABORT, 'aircraft generation requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

CREATE TRIGGER IF NOT EXISTS aircraft_package_retrieval_key_validate_insert
BEFORE INSERT ON aircraft_factory_packages
BEGIN
  SELECT RAISE(ABORT, 'aircraft package requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;
CREATE TRIGGER IF NOT EXISTS aircraft_package_retrieval_key_validate_update
BEFORE UPDATE OF name, normalized_name ON aircraft_factory_packages
BEGIN
  SELECT RAISE(ABORT, 'aircraft package requires its deterministic retrieval key')
  WHERE NEW.normalized_name <> (
    WITH RECURSIVE normalized(character_offset, normalized_name) AS (
      VALUES (1, '')
      UNION ALL
      SELECT
        character_offset + 1,
        CASE
          WHEN substr(NEW.name, character_offset, 1) GLOB '[A-Za-z0-9]'
            THEN normalized_name || lower(substr(NEW.name, character_offset, 1))
          WHEN normalized_name <> '' AND substr(normalized_name, -1, 1) <> ' '
            THEN normalized_name || ' '
          ELSE normalized_name
        END
      FROM normalized
      WHERE character_offset <= length(NEW.name)
    )
    SELECT rtrim(normalized_name)
    FROM normalized
    WHERE character_offset > length(NEW.name)
  );
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260729_aircraft_catalog_retrieval_keys',
  1,
  'b40b266fc450810cf89acc78c9405f4cd7d816ea38d389114e93a20cfea6901d',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

CREATE UNIQUE INDEX IF NOT EXISTS idx_faa_registry_aircraft_lineage_record
  ON faa_registry_aircraft (
    snapshot_id,
    n_number,
    source_record_sha256,
    manufacturer_serial_key,
    aircraft_code
  );

CREATE TABLE IF NOT EXISTS aircraft_tcds_make_lineage_bindings (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  faa_snapshot_date TEXT NOT NULL,
  faa_archive_sha256 TEXT NOT NULL,
  faa_aircraft_code TEXT NOT NULL,
  representative_faa_registry_snapshot_id INTEGER NOT NULL,
  representative_faa_n_number TEXT NOT NULL,
  representative_faa_source_record_sha256 TEXT NOT NULL,
  representative_faa_manufacturer_serial_key TEXT NOT NULL,
  faa_manufacturer_name TEXT NOT NULL,
  faa_model TEXT NOT NULL,
  aircraft_make_id INTEGER NOT NULL
    REFERENCES aircraft_makes(id) ON DELETE RESTRICT,
  aircraft_designation_id INTEGER NOT NULL
    REFERENCES aircraft_designations(id) ON DELETE RESTRICT,
  tcds_number TEXT NOT NULL,
  tcds_document_guid TEXT NOT NULL,
  tcds_pdf_sha256 TEXT NOT NULL,
  tcds_former_holder_name TEXT NOT NULL,
  tcds_current_holder_name TEXT NOT NULL,
  tcds_manufacturer_name TEXT,
  tcds_selection_basis TEXT NOT NULL CHECK (
    tcds_selection_basis IN (
      'registry_reference',
      'drs_unique_current_exact_model',
      'operator_validated_exact_model_serial'
    )
  ),
  serial_scope_kind TEXT NOT NULL
    CHECK (serial_scope_kind IN ('tcds_model', 'manufacturer')),
  serial_prefix TEXT NOT NULL,
  serial_digits_width INTEGER NOT NULL,
  first_serial_number INTEGER NOT NULL,
  last_serial_number INTEGER,
  approval_decision_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_identity_decisions(id) ON DELETE RESTRICT,
  faa_make_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_model_identity_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_serial_applicability_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_holder_transfer_evidence_claim_id INTEGER NOT NULL
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  tcds_manufacturer_range_evidence_claim_id INTEGER
    REFERENCES curation_evidence_claims(id) ON DELETE RESTRICT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  FOREIGN KEY (
    representative_faa_registry_snapshot_id, faa_aircraft_code
  ) REFERENCES faa_registry_aircraft_references(snapshot_id, aircraft_code)
    ON DELETE RESTRICT,
  FOREIGN KEY (
    representative_faa_registry_snapshot_id,
    representative_faa_n_number,
    representative_faa_source_record_sha256,
    representative_faa_manufacturer_serial_key,
    faa_aircraft_code
  ) REFERENCES faa_registry_aircraft (
    snapshot_id,
    n_number,
    source_record_sha256,
    manufacturer_serial_key,
    aircraft_code
  ) ON DELETE RESTRICT,
  CHECK (
    faa_snapshot_date
      GLOB '[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]'
  ),
  CHECK (
    length(faa_archive_sha256) = 64
    AND faa_archive_sha256 NOT GLOB '*[^0-9a-f]*'
  ),
  CHECK (length(trim(faa_aircraft_code)) > 0),
  CHECK (
    substr(representative_faa_n_number, 1, 1) = 'N'
    AND length(representative_faa_n_number) BETWEEN 2 AND 6
  ),
  CHECK (
    length(representative_faa_source_record_sha256) = 64
    AND representative_faa_source_record_sha256 NOT GLOB '*[^0-9a-f]*'
  ),
  CHECK (
    length(representative_faa_manufacturer_serial_key) > 0
    AND representative_faa_manufacturer_serial_key =
      upper(representative_faa_manufacturer_serial_key)
  ),
  CHECK (
    length(trim(faa_manufacturer_name)) > 0
    AND faa_manufacturer_name = trim(faa_manufacturer_name)
  ),
  CHECK (length(trim(faa_model)) > 0 AND faa_model = trim(faa_model)),
  CHECK (length(trim(tcds_number)) > 0 AND tcds_number = trim(tcds_number)),
  CHECK (
    length(tcds_document_guid) = 36
    AND length(replace(tcds_document_guid, '-', '')) = 32
    AND substr(tcds_document_guid, 9, 1) = '-'
    AND substr(tcds_document_guid, 14, 1) = '-'
    AND substr(tcds_document_guid, 19, 1) = '-'
    AND substr(tcds_document_guid, 24, 1) = '-'
    AND replace(tcds_document_guid, '-', '') NOT GLOB '*[^0-9A-Fa-f]*'
  ),
  CHECK (
    length(tcds_pdf_sha256) = 64
    AND tcds_pdf_sha256 NOT GLOB '*[^0-9a-f]*'
  ),
  CHECK (
    length(trim(tcds_former_holder_name)) > 0
    AND tcds_former_holder_name = trim(tcds_former_holder_name)
    AND length(trim(tcds_current_holder_name)) > 0
    AND tcds_current_holder_name = trim(tcds_current_holder_name)
    AND tcds_former_holder_name <> tcds_current_holder_name
  ),
  CHECK (
    tcds_manufacturer_name IS NULL
    OR (
      length(trim(tcds_manufacturer_name)) > 0
      AND tcds_manufacturer_name = trim(tcds_manufacturer_name)
    )
  ),
  CHECK (
    serial_prefix = upper(serial_prefix)
    AND serial_prefix NOT GLOB '*[^A-Z]*'
    AND length(serial_prefix) <= 16
  ),
  CHECK (
    typeof(serial_digits_width) = 'integer'
    AND serial_digits_width BETWEEN 1 AND 18
  ),
  CHECK (
    typeof(first_serial_number) = 'integer'
    AND first_serial_number >= 0
  ),
  CHECK (
    last_serial_number IS NULL
    OR (
      typeof(last_serial_number) = 'integer'
      AND last_serial_number >= first_serial_number
    )
  ),
  CHECK (
    faa_make_evidence_claim_id <> tcds_model_identity_evidence_claim_id
    AND faa_make_evidence_claim_id <>
      tcds_serial_applicability_evidence_claim_id
    AND faa_make_evidence_claim_id <>
      tcds_holder_transfer_evidence_claim_id
    AND tcds_model_identity_evidence_claim_id <>
      tcds_serial_applicability_evidence_claim_id
    AND tcds_model_identity_evidence_claim_id <>
      tcds_holder_transfer_evidence_claim_id
    AND tcds_serial_applicability_evidence_claim_id <>
      tcds_holder_transfer_evidence_claim_id
    AND (
      tcds_manufacturer_range_evidence_claim_id IS NULL
      OR (
        tcds_manufacturer_range_evidence_claim_id <>
          faa_make_evidence_claim_id
        AND tcds_manufacturer_range_evidence_claim_id <>
          tcds_model_identity_evidence_claim_id
        AND tcds_manufacturer_range_evidence_claim_id <>
          tcds_serial_applicability_evidence_claim_id
        AND tcds_manufacturer_range_evidence_claim_id <>
          tcds_holder_transfer_evidence_claim_id
      )
    )
    AND (
      (
        serial_scope_kind = 'tcds_model'
        AND tcds_manufacturer_name IS NULL
        AND tcds_manufacturer_range_evidence_claim_id IS NULL
      )
      OR (
        serial_scope_kind = 'manufacturer'
        AND tcds_manufacturer_name IS NOT NULL
        AND tcds_manufacturer_range_evidence_claim_id IS NOT NULL
      )
    )
  )
);

DROP INDEX IF EXISTS idx_aircraft_tcds_make_lineage_scope;
CREATE UNIQUE INDEX idx_aircraft_tcds_make_lineage_scope
  ON aircraft_tcds_make_lineage_bindings (
    faa_snapshot_date,
    faa_archive_sha256,
    faa_aircraft_code,
    faa_manufacturer_name,
    faa_model,
    serial_prefix,
    serial_digits_width,
    first_serial_number,
    coalesce(last_serial_number, -1)
  );

CREATE INDEX IF NOT EXISTS idx_aircraft_tcds_make_lineage_lookup
  ON aircraft_tcds_make_lineage_bindings (
    faa_snapshot_date,
    faa_archive_sha256,
    faa_aircraft_code,
    aircraft_designation_id,
    faa_manufacturer_name,
    faa_model,
    serial_prefix,
    serial_digits_width,
    first_serial_number,
    last_serial_number
  );

-- One approved range must be backed by the exact imported FAA record and four
-- distinct claims: FAA make, TCDS model identity, TCDS model/serial
-- applicability, and TCDS holder transfer. A manufacturer-specific serial
-- range is optional strengthening evidence. Matching names are copied
-- literally; this trigger never strips a legal suffix or manufactures a
-- semantic alias.
DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_requires_provenance;
CREATE TRIGGER aircraft_tcds_make_lineage_requires_provenance
BEFORE INSERT ON aircraft_tcds_make_lineage_bindings
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_identity_decisions decision
  JOIN aircraft_identity_decision_claims faa_link
    ON faa_link.decision_id = decision.id
   AND faa_link.evidence_claim_id = NEW.faa_make_evidence_claim_id
   AND faa_link.evidence_role = 'identity'
  JOIN aircraft_identity_decision_claims tcds_model_link
    ON tcds_model_link.decision_id = decision.id
   AND tcds_model_link.evidence_claim_id =
       NEW.tcds_model_identity_evidence_claim_id
   AND tcds_model_link.evidence_role = 'identity'
  JOIN aircraft_identity_decision_claims tcds_serial_link
    ON tcds_serial_link.decision_id = decision.id
   AND tcds_serial_link.evidence_claim_id =
       NEW.tcds_serial_applicability_evidence_claim_id
   AND tcds_serial_link.evidence_role = 'applicability'
  JOIN aircraft_identity_decision_claims tcds_holder_link
    ON tcds_holder_link.decision_id = decision.id
   AND tcds_holder_link.evidence_claim_id =
       NEW.tcds_holder_transfer_evidence_claim_id
   AND tcds_holder_link.evidence_role = 'identity'
  JOIN curation_evidence_claims faa_claim
    ON faa_claim.id = NEW.faa_make_evidence_claim_id
  JOIN curation_evidence_sources faa_source
    ON faa_source.id = faa_claim.evidence_source_id
  JOIN curation_evidence_claims tcds_model_claim
    ON tcds_model_claim.id = NEW.tcds_model_identity_evidence_claim_id
  JOIN curation_evidence_sources tcds_model_source
    ON tcds_model_source.id = tcds_model_claim.evidence_source_id
  JOIN curation_evidence_claims tcds_serial_claim
    ON tcds_serial_claim.id =
       NEW.tcds_serial_applicability_evidence_claim_id
  JOIN curation_evidence_sources tcds_serial_source
    ON tcds_serial_source.id = tcds_serial_claim.evidence_source_id
  JOIN curation_evidence_claims tcds_holder_claim
    ON tcds_holder_claim.id = NEW.tcds_holder_transfer_evidence_claim_id
  JOIN curation_evidence_sources tcds_holder_source
    ON tcds_holder_source.id = tcds_holder_claim.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.representative_faa_registry_snapshot_id
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = snapshot.id
   AND reference.aircraft_code = NEW.faa_aircraft_code
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  JOIN aircraft_model_families family
    ON family.id = designation.aircraft_model_family_id
   AND family.aircraft_make_id = NEW.aircraft_make_id
  JOIN aircraft_makes canonical_make
    ON canonical_make.id = family.aircraft_make_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  WHERE decision.id = NEW.approval_decision_id
    AND decision.entity_kind = 'make'
    AND decision.decision_action = 'match_existing'
    AND decision.decision_status = 'approved'
    AND decision.deterministic_validation_passed = 1
    AND decision.selected_entity_id = NEW.aircraft_make_id
    AND faa_claim.claim_kind = 'identity'
    AND faa_claim.validation_status = 'validated'
    AND faa_source.id = snapshot.evidence_source_id
    AND faa_source.source_tier = 'regulator_primary'
    AND tcds_model_claim.claim_kind = 'identity'
    AND tcds_model_claim.validation_status = 'validated'
    AND tcds_model_source.source_tier = 'regulator_primary'
    AND tcds_model_source.content_sha256 = NEW.tcds_pdf_sha256
    AND tcds_serial_claim.claim_kind = 'applicability'
    AND tcds_serial_claim.validation_status = 'validated'
    AND tcds_serial_source.id = tcds_model_source.id
    AND tcds_holder_claim.claim_kind = 'identity'
    AND tcds_holder_claim.validation_status = 'validated'
    AND tcds_holder_source.id = tcds_model_source.id
    AND tcds_model_source.source_url =
      'https://drs.faa.gov/api/drs/data-pull/download/'
      || NEW.tcds_document_guid
    AND (
      NEW.tcds_manufacturer_range_evidence_claim_id IS NULL
      OR (
        EXISTS (
          SELECT 1
          FROM aircraft_identity_decision_claims manufacturer_link
          JOIN curation_evidence_claims manufacturer_claim
            ON manufacturer_claim.id = manufacturer_link.evidence_claim_id
          WHERE manufacturer_link.decision_id = decision.id
            AND manufacturer_link.evidence_claim_id =
              NEW.tcds_manufacturer_range_evidence_claim_id
            AND manufacturer_link.evidence_role = 'applicability'
            AND manufacturer_claim.claim_kind = 'applicability'
            AND manufacturer_claim.validation_status = 'validated'
            AND manufacturer_claim.evidence_source_id = tcds_model_source.id
        )
      )
    )
    AND NEW.faa_snapshot_date = snapshot.snapshot_date
    AND NEW.faa_archive_sha256 = snapshot.archive_sha256
    AND NEW.faa_manufacturer_name = reference.manufacturer_name
    AND NEW.faa_model = reference.model_name
    AND (
      (
        NEW.tcds_selection_basis = 'registry_reference'
        AND length(trim(coalesce(reference.type_certificate_data_sheet, ''))) > 0
        AND NEW.tcds_number = trim(reference.type_certificate_data_sheet)
      )
      OR (
        NEW.tcds_selection_basis IN (
          'drs_unique_current_exact_model',
          'operator_validated_exact_model_serial'
        )
        AND length(trim(coalesce(reference.type_certificate_data_sheet, ''))) = 0
      )
    )
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(rtrim(trim(canonical_make.name), '.')) =
        lower(rtrim(trim(NEW.tcds_former_holder_name), '.'))
      OR lower(rtrim(trim(canonical_make.name), '.')) =
        lower(rtrim(trim(NEW.tcds_current_holder_name), '.'))
    )
    AND (
      NEW.tcds_manufacturer_name IS NULL
      OR lower(rtrim(trim(NEW.tcds_manufacturer_name), '.')) =
          lower(rtrim(trim(NEW.tcds_former_holder_name), '.'))
      OR lower(rtrim(trim(NEW.tcds_manufacturer_name), '.')) =
          lower(rtrim(trim(NEW.tcds_current_holder_name), '.'))
    )
    AND EXISTS (
      SELECT 1
      FROM faa_registry_aircraft aircraft
      WHERE aircraft.snapshot_id = snapshot.id
        AND aircraft.n_number = NEW.representative_faa_n_number
        AND aircraft.source_record_sha256 =
          NEW.representative_faa_source_record_sha256
        AND aircraft.manufacturer_serial_key =
          NEW.representative_faa_manufacturer_serial_key
        AND aircraft.aircraft_code = NEW.faa_aircraft_code
        AND aircraft.manufacturer_serial_key IS NOT NULL
        AND length(aircraft.manufacturer_serial_key) =
          length(NEW.serial_prefix) + NEW.serial_digits_width
        AND substr(
          aircraft.manufacturer_serial_key, 1, length(NEW.serial_prefix)
        ) = NEW.serial_prefix
        AND substr(
          aircraft.manufacturer_serial_key, length(NEW.serial_prefix) + 1
        ) NOT GLOB '*[^0-9]*'
        AND CAST(substr(
          aircraft.manufacturer_serial_key, length(NEW.serial_prefix) + 1
        ) AS INTEGER) >= NEW.first_serial_number
        AND (
          NEW.last_serial_number IS NULL
          OR CAST(substr(
            aircraft.manufacturer_serial_key, length(NEW.serial_prefix) + 1
          ) AS INTEGER) <= NEW.last_serial_number
        )
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA/TCDS make lineage requires distinct FAA make, TCDS model, serial-applicability, and holder-transfer evidence'
  );
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_no_overlap;
CREATE TRIGGER aircraft_tcds_make_lineage_no_overlap
BEFORE INSERT ON aircraft_tcds_make_lineage_bindings
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings existing
  WHERE existing.faa_snapshot_date = NEW.faa_snapshot_date
    AND existing.faa_archive_sha256 = NEW.faa_archive_sha256
    AND existing.faa_aircraft_code = NEW.faa_aircraft_code
    AND existing.faa_manufacturer_name = NEW.faa_manufacturer_name
    AND existing.faa_model = NEW.faa_model
    AND existing.serial_prefix = NEW.serial_prefix
    AND existing.serial_digits_width = NEW.serial_digits_width
    AND (
      existing.last_serial_number IS NULL
      OR existing.last_serial_number >= NEW.first_serial_number
    )
    AND (
      NEW.last_serial_number IS NULL
      OR NEW.last_serial_number >= existing.first_serial_number
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA/TCDS make-lineage serial ranges cannot overlap'
  );
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_no_catalog_collision;
CREATE TRIGGER aircraft_tcds_make_lineage_no_catalog_collision
BEFORE INSERT ON aircraft_tcds_make_lineage_bindings
WHEN EXISTS (
  SELECT 1
  FROM aircraft_makes other_make
  WHERE other_make.id <> NEW.aircraft_make_id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(other_make.name), ' ', ''), '-', ''), '.', ''),
      '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')',
      '')) =
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(NEW.faa_manufacturer_name), ' ', ''), '-', ''),
        '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
OR EXISTS (
  SELECT 1
  FROM aircraft_make_aliases alias
  WHERE alias.aircraft_make_id <> NEW.aircraft_make_id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.', ''), '/',
      ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
      = lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(NEW.faa_manufacturer_name), ' ', ''), '-', ''),
        '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA/TCDS make lineage collides with another canonical make or alias'
  );
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_immutable_update;
CREATE TRIGGER aircraft_tcds_make_lineage_immutable_update
BEFORE UPDATE ON aircraft_tcds_make_lineage_bindings
BEGIN
  SELECT RAISE(ABORT, 'approved FAA/TCDS make-lineage bindings are immutable');
END;

DROP TRIGGER IF EXISTS aircraft_tcds_make_lineage_immutable_delete;
CREATE TRIGGER aircraft_tcds_make_lineage_immutable_delete
BEFORE DELETE ON aircraft_tcds_make_lineage_bindings
BEGIN
  SELECT RAISE(ABORT, 'approved FAA/TCDS make-lineage bindings are immutable');
END;

DROP TRIGGER IF EXISTS aircraft_make_tcds_lineage_collision_insert;
CREATE TRIGGER aircraft_make_tcds_lineage_collision_insert
BEFORE INSERT ON aircraft_makes
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings binding
  WHERE lower(replace(replace(replace(replace(replace(replace(replace(replace(
    replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''),
    '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', '')) =
    lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(binding.faa_manufacturer_name), ' ', ''), '-', ''),
      '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(',
      ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'canonical aircraft make collides with an approved FAA/TCDS lineage label'
  );
END;

DROP TRIGGER IF EXISTS aircraft_make_tcds_lineage_collision_update;
CREATE TRIGGER aircraft_make_tcds_lineage_collision_update
BEFORE UPDATE OF name, normalized_name ON aircraft_makes
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings binding
  WHERE binding.aircraft_make_id <> OLD.id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(NEW.name), ' ', ''), '-', ''), '.', ''), '/', ''),
      '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', '')) =
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(binding.faa_manufacturer_name), ' ', ''), '-',
        ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'canonical aircraft make collides with an approved FAA/TCDS lineage label'
  );
END;

DROP TRIGGER IF EXISTS aircraft_make_alias_tcds_lineage_collision;
CREATE TRIGGER aircraft_make_alias_tcds_lineage_collision
BEFORE INSERT ON aircraft_make_aliases
WHEN EXISTS (
  SELECT 1
  FROM aircraft_tcds_make_lineage_bindings binding
  WHERE binding.aircraft_make_id <> NEW.aircraft_make_id
    AND lower(replace(replace(replace(replace(replace(replace(replace(replace(
      replace(replace(trim(NEW.alias), ' ', ''), '-', ''), '.', ''), '/', ''),
      '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', '')) =
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(binding.faa_manufacturer_name), ' ', ''), '-',
        ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
        '(', ''), ')', ''))
)
BEGIN
  SELECT RAISE(
    ABORT,
    'aircraft make alias collides with an approved FAA/TCDS lineage label'
  );
END;

-- The three admission barriers below replace their year-alias-only versions.
-- Every TCDS branch repeats the exact FAA release/code/model/manufacturer and
-- serial-range match; possession of an unrelated binding is never sufficient.

DROP TRIGGER IF EXISTS aircraft_designation_faa_binding_requires_provenance;
CREATE TRIGGER aircraft_designation_faa_binding_requires_provenance
BEFORE INSERT ON aircraft_designation_faa_bindings
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_designations designation
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_model_families family
    ON family.id = designation.aircraft_model_family_id
  JOIN aircraft_makes make
    ON make.id = family.aircraft_make_id
  JOIN aircraft_identity_decisions decision
    ON decision.id = designation.approval_decision_id
  JOIN curation_evidence_claims claim
    ON claim.id = NEW.identity_evidence_claim_id
  JOIN curation_evidence_sources source
    ON source.id = claim.evidence_source_id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = NEW.representative_faa_registry_snapshot_id
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = snapshot.id
   AND reference.aircraft_code = NEW.faa_aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  WHERE designation.id = NEW.aircraft_designation_id
    AND decision.decision_status = 'approved'
    AND decision.decision_action = 'approve_new'
    AND decision.entity_kind = 'designation'
    AND claim.claim_kind = 'identity'
    AND claim.validation_status = 'validated'
    AND source.id = snapshot.evidence_source_id
    AND source.source_tier = 'regulator_primary'
    AND NEW.faa_snapshot_date = snapshot.snapshot_date
    AND NEW.faa_archive_sha256 = snapshot.archive_sha256
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/',
        ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(
          replace(replace(trim(reference.manufacturer_name), ' ', ''), '-',
          ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39),
          ''), '(', ''), ')', ''))
      OR (
        EXISTS (
          SELECT 1
          FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
        )
        AND NOT EXISTS (
          SELECT 1
          FROM faa_registry_aircraft registered_aircraft
          WHERE registered_aircraft.snapshot_id = snapshot.id
            AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
            AND NOT EXISTS (
              SELECT 1
              FROM aircraft_make_aliases alias
              LEFT JOIN aircraft_markets market
                ON market.id = alias.aircraft_market_id
              WHERE alias.aircraft_make_id = make.id
                AND lower(replace(replace(replace(replace(replace(replace(
                  replace(replace(replace(replace(trim(alias.alias), ' ', ''),
                  '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''),
                  char(39), ''), '(', ''), ')', '')) =
                  lower(replace(replace(replace(replace(replace(replace(
                    replace(replace(replace(replace(
                    trim(reference.manufacturer_name), ' ', ''), '-', ''), '.',
                    ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''),
                    '(', ''), ')', ''))
                AND (
                  alias.aircraft_market_id IS NULL
                  OR market.code IN ('GLOBAL', 'US')
                )
                AND (
                  (
                    registered_aircraft.year_manufactured IS NULL
                    AND alias.valid_from_model_year IS NULL
                    AND alias.valid_to_model_year IS NULL
                  )
                  OR (
                    registered_aircraft.year_manufactured IS NOT NULL
                    AND (
                      alias.valid_from_model_year IS NULL
                      OR alias.valid_from_model_year <=
                         registered_aircraft.year_manufactured
                    )
                    AND (
                      alias.valid_to_model_year IS NULL
                      OR alias.valid_to_model_year >=
                         registered_aircraft.year_manufactured
                    )
                  )
                )
            )
        )
      )
      OR EXISTS (
        SELECT 1
        FROM aircraft_tcds_make_lineage_bindings binding
        JOIN faa_registry_aircraft registered_aircraft
          ON registered_aircraft.snapshot_id = snapshot.id
         AND registered_aircraft.aircraft_code = NEW.faa_aircraft_code
        WHERE binding.faa_snapshot_date = NEW.faa_snapshot_date
          AND binding.faa_archive_sha256 = NEW.faa_archive_sha256
          AND binding.faa_aircraft_code = NEW.faa_aircraft_code
          AND binding.faa_manufacturer_name = reference.manufacturer_name
          AND binding.faa_model = reference.model_name
          AND binding.aircraft_make_id = make.id
          AND binding.aircraft_designation_id = designation.id
          AND registered_aircraft.manufacturer_serial_key IS NOT NULL
          AND length(registered_aircraft.manufacturer_serial_key) =
            length(binding.serial_prefix) + binding.serial_digits_width
          AND substr(
            registered_aircraft.manufacturer_serial_key,
            1,
            length(binding.serial_prefix)
          ) = binding.serial_prefix
          AND substr(
            registered_aircraft.manufacturer_serial_key,
            length(binding.serial_prefix) + 1
          ) NOT GLOB '*[^0-9]*'
          AND CAST(substr(
            registered_aircraft.manufacturer_serial_key,
            length(binding.serial_prefix) + 1
          ) AS INTEGER) >= binding.first_serial_number
          AND (
            binding.last_serial_number IS NULL
            OR CAST(substr(
              registered_aircraft.manufacturer_serial_key,
              length(binding.serial_prefix) + 1
            ) AS INTEGER) <= binding.last_serial_number
          )
      )
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'FAA aircraft code binding requires an exact approved designation, applicable manufacturer identity, and regulator evidence'
  );
END;

DROP TRIGGER IF EXISTS listing_identity_assignment_requires_faa_identity;
CREATE TRIGGER listing_identity_assignment_requires_faa_identity
BEFORE INSERT ON aircraft_sale_listing_identity_assignments
WHEN NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listings listing
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = NEW.faa_registry_snapshot_id
   AND aircraft.n_number = NEW.faa_n_number
   AND aircraft.source_record_sha256 = NEW.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_snapshots registry_snapshot
    ON registry_snapshot.id = aircraft.snapshot_id
  JOIN aircraft_designations designation
    ON designation.id = NEW.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = designation.id
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = registry_snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = registry_snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id = designation.id
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_makes make
    ON make.id = NEW.aircraft_make_id
  WHERE listing.id = NEW.aircraft_sale_listing_id
    AND upper(replace(replace(trim(listing.registration_number), '-', ''), ' ', ''))
      = NEW.faa_n_number
    AND length(trim(reference.manufacturer_name)) > 0
    AND length(trim(reference.model_name)) > 0
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(make.name), ' ', ''), '-', ''), '.', ''), '/',
        ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''), ')', ''))
        = lower(replace(replace(replace(replace(replace(replace(replace(replace(
          replace(replace(trim(reference.manufacturer_name), ' ', ''), '-',
          ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39),
          ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(
            replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.',
            ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(',
            ''), ')', '')) =
            lower(replace(replace(replace(replace(replace(replace(replace(
              replace(replace(replace(trim(reference.manufacturer_name), ' ',
              ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''),
              char(39), ''), '(', ''), ')', ''))
          AND (
            alias.aircraft_market_id IS NULL
            OR market.code IN ('GLOBAL', 'US')
          )
          AND (
            alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= listing.model_year
          )
          AND (
            alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= listing.model_year
          )
      )
      OR EXISTS (
        SELECT 1
        FROM aircraft_tcds_make_lineage_bindings binding
        WHERE binding.faa_snapshot_date = registry_snapshot.snapshot_date
          AND binding.faa_archive_sha256 = registry_snapshot.archive_sha256
          AND binding.faa_aircraft_code = aircraft.aircraft_code
          AND binding.faa_manufacturer_name = reference.manufacturer_name
          AND binding.faa_model = reference.model_name
          AND binding.aircraft_make_id = make.id
          AND binding.aircraft_designation_id = designation.id
          AND aircraft.manufacturer_serial_key IS NOT NULL
          AND length(aircraft.manufacturer_serial_key) =
            length(binding.serial_prefix) + binding.serial_digits_width
          AND substr(
            aircraft.manufacturer_serial_key, 1, length(binding.serial_prefix)
          ) = binding.serial_prefix
          AND substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) NOT GLOB '*[^0-9]*'
          AND CAST(substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) AS INTEGER) >= binding.first_serial_number
          AND (
            binding.last_serial_number IS NULL
            OR CAST(substr(
              aircraft.manufacturer_serial_key,
              length(binding.serial_prefix) + 1
            ) AS INTEGER) <= binding.last_serial_number
          )
      )
    )
    AND designation_key.identity_key = reference_key.identity_key
)
BEGIN
  SELECT RAISE(
    ABORT,
    'listing aircraft assignment designation does not match the exact FAA aircraft identity'
  );
END;

DROP TRIGGER IF EXISTS listing_ready_requires_canonical_aircraft_update;
CREATE TRIGGER listing_ready_requires_canonical_aircraft_update
BEFORE UPDATE OF
  ingestion_state,
  aircraft_model_variant_id,
  model_year,
  registration_number,
  serial_number
ON aircraft_sale_listings
WHEN NEW.ingestion_state = 'ready' AND NOT EXISTS (
  SELECT 1
  FROM aircraft_sale_listing_current_identity_assignments current_assignment
  JOIN aircraft_sale_listing_identity_assignments assignment
    ON assignment.id = current_assignment.identity_assignment_id
   AND assignment.aircraft_sale_listing_id = NEW.id
  JOIN aircraft_makes canonical_make
    ON canonical_make.id = assignment.aircraft_make_id
  JOIN aircraft_designations canonical_designation
    ON canonical_designation.id = assignment.aircraft_designation_id
  JOIN aircraft_designation_identity_keys designation_key
    ON designation_key.aircraft_designation_id = canonical_designation.id
  JOIN faa_registry_snapshots snapshot
    ON snapshot.id = assignment.faa_registry_snapshot_id
  JOIN faa_registry_aircraft aircraft
    ON aircraft.snapshot_id = snapshot.id
   AND aircraft.n_number = assignment.faa_n_number
   AND aircraft.source_record_sha256 = assignment.faa_source_record_sha256
  JOIN faa_registry_aircraft_references reference
    ON reference.snapshot_id = aircraft.snapshot_id
   AND reference.aircraft_code = aircraft.aircraft_code
  JOIN faa_registry_aircraft_reference_identity_keys reference_key
    ON reference_key.faa_registry_snapshot_id = reference.snapshot_id
   AND reference_key.faa_aircraft_code = reference.aircraft_code
  JOIN aircraft_designation_faa_bindings faa_binding
    ON faa_binding.faa_snapshot_date = snapshot.snapshot_date
   AND faa_binding.faa_archive_sha256 = snapshot.archive_sha256
   AND faa_binding.faa_aircraft_code = aircraft.aircraft_code
   AND faa_binding.aircraft_designation_id =
       assignment.aircraft_designation_id
  WHERE current_assignment.aircraft_sale_listing_id = NEW.id
    AND EXISTS (
      SELECT 1
      FROM faa_registry_snapshots latest_release
      WHERE latest_release.id = (
        SELECT id
        FROM faa_registry_snapshots
        ORDER BY snapshot_date DESC, id DESC
        LIMIT 1
      )
        AND latest_release.snapshot_date = snapshot.snapshot_date
        AND latest_release.archive_sha256 = snapshot.archive_sha256
    )
    AND upper(replace(replace(trim(NEW.registration_number), '-', ''), ' ', ''))
      = assignment.faa_n_number
    AND (
      NEW.serial_number IS NULL
      OR trim(NEW.serial_number) = ''
      OR aircraft.manufacturer_serial_raw IS NULL
      OR upper(replace(replace(trim(NEW.serial_number), '-', ''), ' ', '')) =
         upper(replace(replace(
           trim(aircraft.manufacturer_serial_raw), '-', ''
         ), ' ', ''))
    )
    AND designation_key.identity_key = reference_key.identity_key
    AND (
      lower(replace(replace(replace(replace(replace(replace(replace(replace(
        replace(replace(trim(canonical_make.name), ' ', ''), '-', ''), '.',
        ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(', ''),
        ')', '')) =
        lower(replace(replace(replace(replace(replace(replace(replace(replace(
          replace(replace(trim(reference.manufacturer_name), ' ', ''), '-',
          ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39),
          ''), '(', ''), ')', ''))
      OR EXISTS (
        SELECT 1
        FROM aircraft_make_aliases alias
        LEFT JOIN aircraft_markets market
          ON market.id = alias.aircraft_market_id
        WHERE alias.aircraft_make_id = canonical_make.id
          AND lower(replace(replace(replace(replace(replace(replace(replace(
            replace(replace(replace(trim(alias.alias), ' ', ''), '-', ''), '.',
            ''), '/', ''), '_', ''), ',', ''), '&', ''), char(39), ''), '(',
            ''), ')', '')) =
            lower(replace(replace(replace(replace(replace(replace(replace(
              replace(replace(replace(trim(reference.manufacturer_name), ' ',
              ''), '-', ''), '.', ''), '/', ''), '_', ''), ',', ''), '&', ''),
              char(39), ''), '(', ''), ')', ''))
          AND (
            alias.aircraft_market_id IS NULL
            OR market.code IN ('GLOBAL', 'US')
          )
          AND (
            alias.valid_from_model_year IS NULL
            OR alias.valid_from_model_year <= NEW.model_year
          )
          AND (
            alias.valid_to_model_year IS NULL
            OR alias.valid_to_model_year >= NEW.model_year
          )
      )
      OR EXISTS (
        SELECT 1
        FROM aircraft_tcds_make_lineage_bindings binding
        WHERE binding.faa_snapshot_date = snapshot.snapshot_date
          AND binding.faa_archive_sha256 = snapshot.archive_sha256
          AND binding.faa_aircraft_code = aircraft.aircraft_code
          AND binding.faa_manufacturer_name = reference.manufacturer_name
          AND binding.faa_model = reference.model_name
          AND binding.aircraft_make_id = canonical_make.id
          AND binding.aircraft_designation_id =
              canonical_designation.id
          AND aircraft.manufacturer_serial_key IS NOT NULL
          AND length(aircraft.manufacturer_serial_key) =
            length(binding.serial_prefix) + binding.serial_digits_width
          AND substr(
            aircraft.manufacturer_serial_key, 1, length(binding.serial_prefix)
          ) = binding.serial_prefix
          AND substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) NOT GLOB '*[^0-9]*'
          AND CAST(substr(
            aircraft.manufacturer_serial_key, length(binding.serial_prefix) + 1
          ) AS INTEGER) >= binding.first_serial_number
          AND (
            binding.last_serial_number IS NULL
            OR CAST(substr(
              aircraft.manufacturer_serial_key,
              length(binding.serial_prefix) + 1
            ) AS INTEGER) <= binding.last_serial_number
          )
      )
    )
    AND (
      (
        assignment.aircraft_generation_id IS NULL
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_generation_designations generation_link
          WHERE generation_link.aircraft_designation_id =
                assignment.aircraft_designation_id
        )
      )
      OR (
        assignment.aircraft_generation_id IS NOT NULL
        AND EXISTS (
          SELECT 1
          FROM aircraft_generation_designations generation_link
          WHERE generation_link.aircraft_generation_id =
                assignment.aircraft_generation_id
            AND generation_link.aircraft_designation_id =
                assignment.aircraft_designation_id
        )
      )
    )
    AND (
      (
        assignment.aircraft_factory_package_id IS NULL
        AND NOT EXISTS (
          SELECT 1
          FROM aircraft_package_applicability applicability
          JOIN aircraft_factory_packages package
            ON package.id = applicability.aircraft_factory_package_id
          WHERE applicability.aircraft_designation_id =
                assignment.aircraft_designation_id
            AND package.package_kind = 'trim_tier'
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id =
                 assignment.aircraft_generation_id
            )
            AND (
              applicability.valid_from_model_year IS NULL
              OR applicability.valid_from_model_year <= NEW.model_year
            )
            AND (
              applicability.valid_to_model_year IS NULL
              OR applicability.valid_to_model_year >= NEW.model_year
            )
        )
      )
      OR (
        assignment.aircraft_factory_package_id IS NOT NULL
        AND EXISTS (
          SELECT 1
          FROM aircraft_package_applicability applicability
          WHERE applicability.aircraft_factory_package_id =
                assignment.aircraft_factory_package_id
            AND applicability.aircraft_designation_id =
                assignment.aircraft_designation_id
            AND (
              applicability.aircraft_generation_id IS NULL
              OR applicability.aircraft_generation_id =
                 assignment.aircraft_generation_id
            )
            AND (
              applicability.valid_from_model_year IS NULL
              OR applicability.valid_from_model_year <= NEW.model_year
            )
            AND (
              applicability.valid_to_model_year IS NULL
              OR applicability.valid_to_model_year >= NEW.model_year
            )
        )
      )
    )
)
BEGIN
  SELECT RAISE(
    ABORT,
    'ready listing requires a current canonical aircraft assignment matching current FAA identity'
  );
END;

INSERT INTO schema_migration_contracts (
  migration_name,
  contract_version,
  contract_fingerprint,
  installed_at
) VALUES (
  '20260730_aircraft_tcds_make_lineage',
  1,
  '566485027d3df81bb5a90abcc0ce2b707e565bcbdc92ae3f007f527832fae735',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

-- Re-materialize the exact post-DDL object set. Keeping a sqlite_schema-backed
-- view alive across the schema would make every intervening DDL reprepare it.
CREATE TEMP TABLE reference_catalog_schema_owned_objects AS
WITH
protected_relations(name) AS (
  VALUES
    ('plugin_submissions'),
    ('avionics_models'),
    ('aircraft_engine_catalog_models'),
    ('aircraft_propeller_catalog_models'),
    ('aircraft_makes'),
    ('aircraft_model_families'),
    ('aircraft_designations'),
    ('aircraft_make_aliases'),
    ('aircraft_family_aliases'),
    ('aircraft_designation_aliases'),
    ('aircraft_designation_identifiers'),
    ('aircraft_generations'),
    ('aircraft_generation_designations'),
    ('aircraft_factory_packages'),
    ('aircraft_package_applicability'),
    ('aircraft_reference_configurations'),
    ('aircraft_serial_number_schemes'),
    ('aircraft_feature_definitions'),
    ('aircraft_reference_configuration_versions'),
    ('aircraft_reference_applicability_scopes'),
    ('aircraft_reference_prices'),
    ('aircraft_reference_avionics'),
    ('aircraft_reference_engines'),
    ('aircraft_reference_propellers'),
    ('aircraft_reference_features'),
    ('aircraft_reference_fact_set_attestations'),
    ('official_dollar_normalization_facts'),
    ('aircraft_valuation_compatibility_projections'),
    ('listing_verification_run_items')
),
retired_relations(name) AS (
  VALUES
    ('aircraft_model_spec_versions'),
    ('aircraft_model_variant_price_points'),
    ('aircraft_model_variant_default_avionics'),
    ('aircraft_model_variant_default_avionics_candidates'),
    ('depreciation_profiles'),
    ('depreciation_profile_fit_metadata'),
    ('component_depreciation_profiles')
),
owned_relations(name) AS (
  SELECT name FROM protected_relations
  UNION ALL SELECT name FROM retired_relations
)
SELECT
  schema_row.type || ':' || schema_row.name AS object_key,
  COALESCE(lower(replace(replace(replace(replace(
    schema_row.sql, char(9), ''
  ), char(10), ''), char(13), ''), ' ', '')), '') AS definition
FROM sqlite_schema schema_row
WHERE (
  schema_row.type = 'table'
  AND schema_row.name IN (SELECT name FROM owned_relations)
) OR (
  schema_row.name IN (SELECT name FROM retired_relations)
  OR schema_row.tbl_name IN (SELECT name FROM retired_relations)
) OR (
  schema_row.type = 'view'
  AND schema_row.name = 'aircraft_reference_serial_key_errors'
) OR (
  schema_row.type = 'trigger'
  AND schema_row.tbl_name IN (SELECT name FROM owned_relations)
  AND schema_row.name NOT IN (
    'avionics_models_approved_concrete_model_insert',
    'avionics_models_approved_concrete_model_update'
  )
)
UNION ALL
SELECT
  'index:' || relation.name || ':' || index_row.name,
  index_row.[unique] || ':' || index_row.origin || ':' ||
    index_row.partial || ':' || COALESCE(lower(replace(replace(replace(replace(
      (SELECT sql FROM sqlite_schema WHERE type = 'index'
       AND name = index_row.name), char(9), ''
    ), char(10), ''), char(13), ''), ' ', '')), '') || ':' || COALESCE((
      SELECT group_concat(index_column.signature, ',')
      FROM (
        SELECT
          xinfo.seqno || ':' || xinfo.cid || ':' ||
          COALESCE(xinfo.name, '') || ':' || xinfo.desc || ':' ||
          xinfo.coll || ':' || xinfo.key AS signature
        FROM pragma_index_xinfo(index_row.name) xinfo
        ORDER BY xinfo.seqno
      ) index_column
    ), '')
FROM owned_relations relation
JOIN pragma_index_list(relation.name) index_row;

CREATE TEMP TABLE reference_catalog_schema_postflight (
  valid INTEGER NOT NULL CHECK (valid = 1)
);
INSERT INTO reference_catalog_schema_postflight (valid)
SELECT CASE WHEN
  (SELECT count(*) FROM reference_catalog_schema_owned_objects) <>
    213
  OR EXISTS (
    SELECT object_key, definition
    FROM reference_catalog_schema_owned_objects
    EXCEPT
    SELECT object_key, definition
    FROM reference_catalog_schema_expected_objects
  )
  OR EXISTS (
    SELECT object_key, definition
    FROM reference_catalog_schema_expected_objects
    EXCEPT
    SELECT object_key, definition
    FROM reference_catalog_schema_owned_objects
  )
  OR EXISTS (
    SELECT 1 FROM sqlite_schema
    WHERE name IN (
      'aircraft_model_spec_versions',
      'aircraft_model_variant_price_points',
      'aircraft_model_variant_default_avionics',
      'aircraft_model_variant_default_avionics_candidates',
      'depreciation_profiles',
      'depreciation_profile_fit_metadata',
      'component_depreciation_profiles'
    ) OR tbl_name IN (
      'aircraft_model_spec_versions',
      'aircraft_model_variant_price_points',
      'aircraft_model_variant_default_avionics',
      'aircraft_model_variant_default_avionics_candidates',
      'depreciation_profiles',
      'depreciation_profile_fit_metadata',
      'component_depreciation_profiles'
    )
  )
THEN 0 ELSE 1 END;
DROP TABLE reference_catalog_schema_postflight;
DROP TABLE reference_catalog_schema_owned_objects;
DROP TABLE reference_catalog_schema_expected_objects;

-- This completion marker is deliberately last: every cutover relation,
-- trigger, and canonical definition must exist before provenance is recorded.
INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_reference_catalog_cutover', 1,
  'fe31ca0eaae57cfc4ba5c824679bd950fcb98e20d6dd3e686a477fd22d05aab5',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

CREATE TRIGGER IF NOT EXISTS avionics_models_approved_concrete_model_insert
BEFORE INSERT ON avionics_models
WHEN NEW.catalog_status = 'approved'
 AND (
  NEW.normalized_name <> lower(trim(NEW.normalized_name))
  OR NEW.normalized_name GLOB '*[^a-z0-9 ]*'
  OR instr(NEW.normalized_name, '  ') > 0
  OR NEW.normalized_name IN (
  '', 'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
  'avionics', 'avionics suite', 'integrated avionics', 'integrated avionics suite',
  'glass panel', 'flight instruments', 'standard flight instruments',
  'standard vfr avionics', 'standard ifr avionics', 'radio', 'radios', 'nav',
  'com', 'nav com', 'gps nav com', 'navigation system', 'gps', 'autopilot',
  'flight director', 'transponder', 'ads b', 'ads b in', 'ads b out',
  'ads b in out', 'ads b in and out', 'weather radar', 'audio panel',
  'standard audio panel', 'audio controller', 'audio control panel',
  'display', 'flight display', 'pfd', 'mfd', 'pfd mfd', 'navigation indicator',
  'traffic', 'active traffic', 'traffic advisory system', 'datalink',
  'datalink weather', 'xm',
  'xm weather', 'xm radio', 'xm weather radio', 'lightning detection',
  'terrain awareness', 'terrain awareness system', 'terrain avoidance system',
  'taws', 'synthetic vision', 'synthetic vision system', 'svt',
  'safetaxi', 'safe taxi',
  'flitecharts', 'flite charts', 'charts', 'electronic charts',
  'electronic stability and protection', 'electronic stability protection',
  'stability and protection', 'wireless data loading',
  'wireless database loading', 'engine monitor', 'engine fuel monitoring',
  'standby instrument', 'backup instruments', 'elt', 'adf', 'dme', 'ahrs',
  'air data computer', 'radar altimeter', 'magnetometer', 'clock timer', 'waas',
  'waas gps', 'dual waas', 'remote transponder', 'transponder ads b',
  'stormscope', 'standard radio navigation', 'equipment'
  )
 )
BEGIN
  SELECT RAISE(ABORT, 'approved avionics normalized_name must be canonical and concrete; canonicalize, correct, or demote it before retrying migration');
END;

CREATE TRIGGER IF NOT EXISTS avionics_models_approved_concrete_model_update
BEFORE UPDATE OF catalog_status, normalized_name ON avionics_models
WHEN NEW.catalog_status = 'approved'
 AND (
  NEW.normalized_name <> lower(trim(NEW.normalized_name))
  OR NEW.normalized_name GLOB '*[^a-z0-9 ]*'
  OR instr(NEW.normalized_name, '  ') > 0
  OR NEW.normalized_name IN (
  '', 'unknown', 'generic', 'standard', 'factory', 'oem', 'various', 'multiple',
  'avionics', 'avionics suite', 'integrated avionics', 'integrated avionics suite',
  'glass panel', 'flight instruments', 'standard flight instruments',
  'standard vfr avionics', 'standard ifr avionics', 'radio', 'radios', 'nav',
  'com', 'nav com', 'gps nav com', 'navigation system', 'gps', 'autopilot',
  'flight director', 'transponder', 'ads b', 'ads b in', 'ads b out',
  'ads b in out', 'ads b in and out', 'weather radar', 'audio panel',
  'standard audio panel', 'audio controller', 'audio control panel',
  'display', 'flight display', 'pfd', 'mfd', 'pfd mfd', 'navigation indicator',
  'traffic', 'active traffic', 'traffic advisory system', 'datalink',
  'datalink weather', 'xm',
  'xm weather', 'xm radio', 'xm weather radio', 'lightning detection',
  'terrain awareness', 'terrain awareness system', 'terrain avoidance system',
  'taws', 'synthetic vision', 'synthetic vision system', 'svt',
  'safetaxi', 'safe taxi',
  'flitecharts', 'flite charts', 'charts', 'electronic charts',
  'electronic stability and protection', 'electronic stability protection',
  'stability and protection', 'wireless data loading',
  'wireless database loading', 'engine monitor', 'engine fuel monitoring',
  'standby instrument', 'backup instruments', 'elt', 'adf', 'dme', 'ahrs',
  'air data computer', 'radar altimeter', 'magnetometer', 'clock timer', 'waas',
  'waas gps', 'dual waas', 'remote transponder', 'transponder ads b',
  'stormscope', 'standard radio navigation', 'equipment'
  )
 )
BEGIN
  SELECT RAISE(ABORT, 'approved avionics normalized_name must be canonical and concrete; canonicalize, correct, or demote it before retrying migration');
END;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260821_avionics_approved_concrete_model', 1,
  '1305564519a99b0ecdfb85a045b9924bf90a33b2914bb6822a219170d541a5f6',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260824_avionics_generic_feature_labels', 1,
  '366cf90682d11e71293461aca169445a04f8b906d8c15dab6fde76e1dc2384c8',
  CURRENT_TIMESTAMP
)
ON CONFLICT (migration_name) DO NOTHING;
