#!/usr/bin/env bash
set -euo pipefail

repository_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
manifest="$repository_root/migrations/manifest.tsv"

extract_contract_tuple() {
  local file="$1"
  local migration="$2"
  awk -v migration="$migration" '
    /^INSERT INTO (public[.])?schema_migration_contracts/ { capture = 1; values = 0 }
    capture && /VALUES[[:space:]]*[(]/ { values = 1 }
    capture && /;$/ { capture = 0; values = 0 }
    capture && values && index($0, "\047" migration "\047") {
      tuple = $0
      getline
      tuple = tuple " " $0
      getline
      tuple = tuple " " $0
      sub(".*\047" migration "\047[[:space:]]*,", "", tuple)
      split(tuple, fields, ",")
      version = fields[1]
      fingerprint = fields[2]
      gsub(/[^0-9]/, "", version)
      gsub(/[^0-9a-f]/, "", fingerprint)
      print version ":" fingerprint
      exit
    }
  ' "$file"
}

expected_names=(
  20260720_valuation_data_hardening
  20260721_avionics_catalog_curation
  20260721_avionics_multi_type
  20260722_aircraft_reference_catalog
  20260723_gemini_usage_accounting
  20260724_listing_pending_reviews
  20260725_identity_deduplication_postconditions
  20260725_listing_aircraft_identity
  20260726_listing_aircraft_compatibility_projection
  20260727_remove_unused_aircraft_curation_runs
  20260728_aircraft_identity_no_supported_selection
  20260729_aircraft_catalog_retrieval_keys
  20260730_aircraft_tcds_make_lineage
  20260731_avionics_human_reviewed_consolidation
  20260801_avionics_authoritative_source_origins
  20260802_default_avionics_candidate_quarantine
  20260803_avionics_product_reuse_attestations
  20260804_avionics_grounded_evidence_refresh
  20260805_listing_avionics_association_corroborations
  20260806_listing_avionics_collision_closure
  20260807_avionics_product_reuse_v2
  20260808_avionics_descriptive_consolidation
  20260809_listing_verification_runs
  20260810_avionics_grounded_exact_model_consolidation
  20260819_listing_avionics_dispositions
  20260819_aircraft_listing_identity_corrections
  20260819_faa_reference_reachability
  20260820_faa_record_hash_domain
  20260819_listing_replay_runs
  20260819_reference_catalog_cutover
  20260821_avionics_approved_concrete_model
  20260821_aircraft_visual_source_corrections
  20260824_avionics_generic_feature_labels
  20260910_versioned_migration_history
)

manifest_names=()
registered_paths=()
sqlite_count=0
postgres_count=0
receipt_names=()
optional_receipt_names=()
expected_sequence=1

while IFS=$'\t' read -r sequence name applicability execution sqlite_sha postgres_sha contract_version contract_fingerprint contract_policy; do
  [[ "$sequence" == \#* ]] && continue
  [[ -n "$sequence" ]] || continue
  [[ "$sequence" == "$expected_sequence" ]] || {
    echo "migration sequence is not contiguous at $name" >&2
    exit 1
  }
  [[ "$name" =~ ^[0-9][a-z0-9_]*$ ]] || {
    echo "unsafe migration logical name: $name" >&2
    exit 1
  }
  [[ "$postgres_sha" =~ ^[0-9a-f]{64}$ ]] || {
    echo "invalid PostgreSQL digest for $name" >&2
    exit 1
  }
  case "$contract_policy" in
    none)
      [[ "$contract_version:$contract_fingerprint" == "-:-" ]] || {
        echo "none contract policy has receipt metadata for $name" >&2
        exit 1
      }
      ;;
    required|optional_exact)
      [[ "$contract_version" =~ ^[1-9][0-9]*$ && \
         "$contract_fingerprint" =~ ^[0-9a-f]{64}$ ]] || {
        echo "invalid contract-receipt metadata for $name" >&2
        exit 1
      }
      if [[ "$contract_policy" == required ]]; then
        receipt_names+=("$name")
      else
        optional_receipt_names+=("$name")
        (( sequence < 34 )) && [[ "$execution" == adopt_only ]] || {
          echo "optional receipt is not adopt-only history: $name" >&2
          exit 1
        }
      fi
      ;;
    *)
      echo "unknown contract-receipt policy for $name: $contract_policy" >&2
      exit 1
      ;;
  esac
  if (( sequence <= 33 )); then
    [[ "$execution" == adopt_only ]] || {
      echo "historical migration is executable: $name" >&2
      exit 1
    }
  else
    [[ "$execution" == atomic ]] || {
      echo "executable suffix is not runner-atomic: $name" >&2
      exit 1
    }
  fi

  postgres_path="$repository_root/migrations/$name.postgres.sql"
  [[ -f "$postgres_path" && ! -L "$postgres_path" ]] || {
    echo "missing regular PostgreSQL migration: $postgres_path" >&2
    exit 1
  }
  [[ "$(sha256sum "$postgres_path" | awk '{print $1}')" == "$postgres_sha" ]] || {
    echo "PostgreSQL migration digest drift: $name" >&2
    exit 1
  }
  if [[ "$contract_policy" != none ]]; then
    [[ "$(extract_contract_tuple "$postgres_path" "$name")" == \
      "$contract_version:$contract_fingerprint" ]] || {
      echo "PostgreSQL migration receipt tuple drift: $name" >&2
      exit 1
    }
  fi
  registered_paths+=("${postgres_path#"$repository_root/"}")
  ((postgres_count += 1))

  case "$applicability" in
    both)
      [[ "$sqlite_sha" =~ ^[0-9a-f]{64}$ ]] || {
        echo "invalid SQLite digest for $name" >&2
        exit 1
      }
      sqlite_path="$repository_root/migrations/$name.sqlite.sql"
      [[ -f "$sqlite_path" && ! -L "$sqlite_path" ]] || {
        echo "missing regular SQLite migration: $sqlite_path" >&2
        exit 1
      }
      [[ "$(sha256sum "$sqlite_path" | awk '{print $1}')" == "$sqlite_sha" ]] || {
        echo "SQLite migration digest drift: $name" >&2
        exit 1
      }
      if [[ "$contract_policy" != none ]]; then
        [[ "$(extract_contract_tuple "$sqlite_path" "$name")" == \
          "$contract_version:$contract_fingerprint" ]] || {
          echo "SQLite migration receipt tuple drift: $name" >&2
          exit 1
        }
      fi
      registered_paths+=("${sqlite_path#"$repository_root/"}")
      ((sqlite_count += 1))
      ;;
    postgres_only)
      [[ "$sequence:$name:$sqlite_sha" == \
        "27:20260819_faa_reference_reachability:-" ]] || {
        echo "unexpected PostgreSQL-only migration: $sequence:$name" >&2
        exit 1
      }
      ;;
    *)
      echo "unknown backend applicability for $name: $applicability" >&2
      exit 1
      ;;
  esac
  manifest_names+=("$name")
  ((expected_sequence += 1))
done < "$manifest"

expected_receipt_names=(
  20260725_identity_deduplication_postconditions
  20260725_listing_aircraft_identity
  20260726_listing_aircraft_compatibility_projection
  20260728_aircraft_identity_no_supported_selection
  20260729_aircraft_catalog_retrieval_keys
  20260730_aircraft_tcds_make_lineage
  20260731_avionics_human_reviewed_consolidation
  20260801_avionics_authoritative_source_origins
  20260803_avionics_product_reuse_attestations
  20260804_avionics_grounded_evidence_refresh
  20260807_avionics_product_reuse_v2
  20260808_avionics_descriptive_consolidation
  20260810_avionics_grounded_exact_model_consolidation
  20260819_aircraft_listing_identity_corrections
  20260819_faa_reference_reachability
  20260820_faa_record_hash_domain
  20260819_listing_replay_runs
  20260819_reference_catalog_cutover
  20260821_avionics_approved_concrete_model
  20260821_aircraft_visual_source_corrections
  20260824_avionics_generic_feature_labels
  20260910_versioned_migration_history
)

diff -u \
  <(printf '%s\n' "${expected_receipt_names[@]}") \
  <(printf '%s\n' "${receipt_names[@]}")

expected_optional_receipt_names=(
  20260802_default_avionics_candidate_quarantine
  20260805_listing_avionics_association_corroborations
  20260806_listing_avionics_collision_closure
  20260809_listing_verification_runs
)

diff -u \
  <(printf '%s\n' "${expected_optional_receipt_names[@]}") \
  <(printf '%s\n' "${optional_receipt_names[@]}")

[[ "${#manifest_names[@]}" -eq 34 && "$sqlite_count" -eq 33 && \
   "$postgres_count" -eq 34 && "${#registered_paths[@]}" -eq 67 ]] || {
  echo "expected 34 logical / 33 SQLite / 34 PostgreSQL / 67 total migrations" >&2
  exit 1
}

diff -u \
  <(printf '%s\n' "${expected_names[@]}") \
  <(printf '%s\n' "${manifest_names[@]}")

diff -u \
  <(printf '%s\n' "${registered_paths[@]}" | LC_ALL=C sort) \
  <(find "$repository_root/migrations" -maxdepth 1 -type f \
      \( -name '*.sqlite.sql' -o -name '*.postgres.sql' \) \
      -printf 'migrations/%f\n' | LC_ALL=C sort)

echo "Versioned migration manifest: 34 logical, 33 SQLite, 34 PostgreSQL, 67 exact SHA-256 files"
