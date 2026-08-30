#!/bin/bash
set -euo pipefail
IFS=$'\n\t'
umask 077

fail() {
  printf 'macOS scheduler helper build security regression failed: %s\n' "$1" >&2
  exit 1
}

script_directory="$(
  CDPATH= cd -- "$(/usr/bin/dirname -- "${BASH_SOURCE[0]}")" && /bin/pwd -P
)"
repository_root="$(CDPATH= cd -- "${script_directory}/../.." && /bin/pwd -P)"
/bin/mkdir -m 0700 -p "${repository_root}/target"
temporary_root="$(
  /usr/bin/mktemp -d \
    "${repository_root}/target/.dayweave-macos-helper-security.XXXXXXXX"
)"

cleanup() {
  local status=$?
  trap - EXIT HUP INT TERM
  case "$temporary_root" in
    "${repository_root}/target"/.dayweave-macos-helper-security.*)
      if test -d "$temporary_root" && test ! -L "$temporary_root"; then
        /bin/rm -rf -- "$temporary_root" || status=1
      fi
      ;;
    *) status=1 ;;
  esac
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM

write_package_manifest() {
  local destination="$1"
  local package_name="$2"
  local with_itoa="$3"

  {
    printf '%s\n' \
      '[package]' \
      "name = \"${package_name}\"" \
      'version = "0.1.0"' \
      'edition = "2024"'
    if test "$with_itoa" = yes; then
      printf '%s\n' '' '[dependencies]' 'itoa = "1.0.18"'
    fi
  } >"$destination"
}

create_fixture() {
  local fixture_root="$1"
  local helper_source_mode="$2"

  /bin/mkdir -m 0700 -p \
    "${fixture_root}/scripts" \
    "${fixture_root}/crates/dayweave-codex/src" \
    "${fixture_root}/crates/dayweave-compose/src" \
    "${fixture_root}/crates/dayweave-core/src" \
    "${fixture_root}/crates/dayweave-google/src" \
    "${fixture_root}/crates/dayweave-scheduler-helper/src" \
    "${fixture_root}/server/dayweave-api/src"
  /usr/bin/install -m 0700 \
    "${repository_root}/scripts/build-macos-scheduler-helper.sh" \
    "${fixture_root}/scripts/build-macos-scheduler-helper.sh"

  {
    printf '%s\n' \
      '[workspace]' \
      'resolver = "2"' \
      'members = [' \
      '  "crates/dayweave-codex",' \
      '  "crates/dayweave-compose",' \
      '  "crates/dayweave-core",' \
      '  "crates/dayweave-google",' \
      '  "crates/dayweave-scheduler-helper",' \
      '  "server/dayweave-api",' \
      ']'
  } >"${fixture_root}/Cargo.toml"
  {
    printf '%s\n' \
      'version = 4' \
      '' \
      '[[package]]' \
      'name = "dayweave-api"' \
      'version = "0.1.0"' \
      '' \
      '[[package]]' \
      'name = "dayweave-codex"' \
      'version = "0.1.0"' \
      '' \
      '[[package]]' \
      'name = "dayweave-compose"' \
      'version = "0.1.0"' \
      '' \
      '[[package]]' \
      'name = "dayweave-core"' \
      'version = "0.1.0"' \
      '' \
      '[[package]]' \
      'name = "dayweave-google"' \
      'version = "0.1.0"' \
      '' \
      '[[package]]' \
      'name = "dayweave-scheduler-helper"' \
      'version = "0.1.0"' \
      'dependencies = [' \
      ' "itoa",' \
      ']' \
      '' \
      '[[package]]' \
      'name = "itoa"' \
      'version = "1.0.18"' \
      'source = "registry+https://github.com/rust-lang/crates.io-index"' \
      'checksum = "8f42a60cbdf9a97f5d2305f08a87dc4e09308d1276d28c869c684d7777685682"'
  } >"${fixture_root}/Cargo.lock"
  {
    printf '%s\n' \
      '[toolchain]' \
      'channel = "1.95.0"' \
      'profile = "minimal"'
  } >"${fixture_root}/rust-toolchain.toml"

  write_package_manifest \
    "${fixture_root}/crates/dayweave-codex/Cargo.toml" dayweave-codex no
  write_package_manifest \
    "${fixture_root}/crates/dayweave-compose/Cargo.toml" dayweave-compose no
  write_package_manifest \
    "${fixture_root}/crates/dayweave-core/Cargo.toml" dayweave-core no
  write_package_manifest \
    "${fixture_root}/crates/dayweave-google/Cargo.toml" dayweave-google no
  write_package_manifest \
    "${fixture_root}/crates/dayweave-scheduler-helper/Cargo.toml" \
    dayweave-scheduler-helper yes
  write_package_manifest \
    "${fixture_root}/server/dayweave-api/Cargo.toml" dayweave-api no

  for library_source in \
    "${fixture_root}/crates/dayweave-codex/src/lib.rs" \
    "${fixture_root}/crates/dayweave-compose/src/lib.rs" \
    "${fixture_root}/crates/dayweave-core/src/lib.rs" \
    "${fixture_root}/crates/dayweave-google/src/lib.rs" \
    "${fixture_root}/server/dayweave-api/src/lib.rs"
  do
    printf '%s\n' 'pub fn synthetic_fixture() {}' >"$library_source"
  done
  if test "$helper_source_mode" = valid; then
    printf '%s\n' \
      'fn main() {' \
      '    let mut buffer = itoa::Buffer::new();' \
      '    let _ = buffer.format(15);' \
      '}' >"${fixture_root}/crates/dayweave-scheduler-helper/src/main.rs"
  else
    printf '%s\n' 'fn main() { this_does_not_compile }' \
      >"${fixture_root}/crates/dayweave-scheduler-helper/src/main.rs"
  fi
}

assert_no_private_roots() {
  local fixture_root="$1"
  local leftovers
  leftovers="$(
    /usr/bin/find "${fixture_root}/target" -maxdepth 1 \
      -name '.dayweave-scheduler-helper.*' -print 2>/dev/null
  )" || true
  test -z "$leftovers" || fail 'a complete private build root was not cleaned'
}

assert_rejected() {
  local label="$1"
  local fixture_root="$2"
  local invocation_path="$3"
  local expected_message="$4"
  local output_log="${temporary_root}/${label}.log"
  local status

  set +e
  PATH='/usr/bin:/bin:/usr/sbin:/sbin' \
    "$invocation_path" >"$output_log" 2>&1
  status=$?
  set -e

  test "$status" -ne 0 || fail "${label} unexpectedly succeeded"
  /usr/bin/grep -Fq -- "$expected_message" "$output_log" \
    || fail "${label} returned the wrong failure"
  assert_no_private_roots "$fixture_root"
}

symlink_fixture="${temporary_root}/symlink-fixture"
create_fixture "$symlink_fixture" valid
/bin/ln -s "$symlink_fixture" "${temporary_root}/symlink-repository"
assert_rejected \
  symlink-component \
  "$symlink_fixture" \
  "${temporary_root}/symlink-repository/scripts/build-macos-scheduler-helper.sh" \
  'without symbolic-link components'

hardlink_input_fixture="${temporary_root}/hardlink-input-fixture"
create_fixture "$hardlink_input_fixture" valid
/bin/ln \
  "${hardlink_input_fixture}/Cargo.lock" \
  "${hardlink_input_fixture}/Cargo.lock.alias"
assert_rejected \
  hardlinked-input \
  "$hardlink_input_fixture" \
  "${hardlink_input_fixture}/scripts/build-macos-scheduler-helper.sh" \
  'a Rust build input must not have hard-linked aliases'

writable_input_fixture="${temporary_root}/writable-input-fixture"
create_fixture "$writable_input_fixture" valid
/bin/chmod 0660 "${writable_input_fixture}/Cargo.lock"
assert_rejected \
  writable-input \
  "$writable_input_fixture" \
  "${writable_input_fixture}/scripts/build-macos-scheduler-helper.sh" \
  'a Rust build input must not be group- or world-writable'

output_symlink_fixture="${temporary_root}/output-symlink-fixture"
create_fixture "$output_symlink_fixture" valid
/bin/mkdir -m 0700 "${output_symlink_fixture}.outside-target"
/bin/ln -s "${output_symlink_fixture}.outside-target" "${output_symlink_fixture}/target"
assert_rejected \
  symlinked-output \
  "$output_symlink_fixture" \
  "${output_symlink_fixture}/scripts/build-macos-scheduler-helper.sh" \
  'the target output directory must be an absolute path without symbolic-link components'

hardlink_output_fixture="${temporary_root}/hardlink-output-fixture"
create_fixture "$hardlink_output_fixture" valid
hardlink_output="${hardlink_output_fixture}/target/aarch64-apple-darwin/release/dayweave-scheduler-helper"
/bin/mkdir -m 0700 -p "${hardlink_output%/*}"
printf '%s\n' existing-helper >"$hardlink_output"
/bin/chmod 0700 "$hardlink_output"
/bin/ln "$hardlink_output" "${hardlink_output_fixture}/existing-helper.alias"
assert_rejected \
  hardlinked-output \
  "$hardlink_output_fixture" \
  "${hardlink_output_fixture}/scripts/build-macos-scheduler-helper.sh" \
  'the existing helper output must not have hard-linked aliases'

writable_ancestor_fixture="${temporary_root}/writable-ancestor-fixture"
create_fixture "$writable_ancestor_fixture" valid
/bin/chmod 0770 "$writable_ancestor_fixture"
assert_rejected \
  writable-ancestor \
  "$writable_ancestor_fixture" \
  "${writable_ancestor_fixture}/scripts/build-macos-scheduler-helper.sh" \
  'the build script has a group- or world-writable path component'
/bin/chmod 0700 "$writable_ancestor_fixture"

compile_failure_fixture="${temporary_root}/compile-failure-fixture"
create_fixture "$compile_failure_fixture" invalid
assert_rejected \
  compile-failure-cleanup \
  "$compile_failure_fixture" \
  "${compile_failure_fixture}/scripts/build-macos-scheduler-helper.sh" \
  'cargo did not complete the scheduler helper build'

hostile_tools="${temporary_root}/hostile-tools"
hostile_home="${temporary_root}/hostile-home"
hostile_cargo_home="${temporary_root}/hostile-cargo-home"
hostile_rustup_home="${temporary_root}/hostile-rustup-home"
hostile_marker="${temporary_root}/hostile-injection-ran"
/bin/mkdir -m 0700 -p \
  "$hostile_tools" "${hostile_home}/.cargo/bin" \
  "$hostile_cargo_home" "$hostile_rustup_home"
for hostile_tool in rustup cargo rustc wrapper linker; do
  {
    printf '%s\n' '#!/bin/bash' 'set -euo pipefail'
    printf 'marker=%q\n' "$hostile_marker"
    printf '%s\n' ': >"$marker"' 'exit 95'
  } >"${hostile_tools}/${hostile_tool}"
  /bin/chmod 0700 "${hostile_tools}/${hostile_tool}"
done
{
  printf '%s\n' 'package DayWeaveHostile;'
  printf 'BEGIN { open(my $marker, ">", q{%s}) or die $!; close($marker); }\n' \
    "$hostile_marker"
  printf '%s\n' '1;'
} >"${hostile_tools}/DayWeaveHostile.pm"
/usr/bin/install -m 0700 \
  "${hostile_tools}/rustup" "${hostile_home}/.cargo/bin/rustup"

config_fixture="${temporary_root}/config-fixture"
create_fixture "$config_fixture" valid
for hostile_config_root in \
  "${temporary_root}/.cargo" \
  "${config_fixture}/.cargo" \
  "$hostile_cargo_home"
do
  /bin/mkdir -m 0700 -p "$hostile_config_root"
  {
    printf '%s\n' \
      '[build]' \
      "rustc-wrapper = \"${hostile_tools}/wrapper\"" \
      'rustflags = ["--definitely-invalid-rustflag"]' \
      '' \
      '[target.aarch64-apple-darwin]' \
      "linker = \"${hostile_tools}/linker\"" \
      '' \
      '[source.crates-io]' \
      'replace-with = "hostile"' \
      '' \
      '[source.hostile]' \
      'directory = "/definitely/missing/dayweave-hostile-source"'
  } >"${hostile_config_root}/config.toml"
done
printf '%s\n' 'token = "synthetic-credential-must-not-leak"' \
  >"${hostile_cargo_home}/credentials.toml"

config_log="${temporary_root}/config-isolation.log"
set +e
PATH="${hostile_tools}:/usr/bin:/bin:/usr/sbin:/sbin" \
  HOME="$hostile_home" \
  CARGO_HOME="$hostile_cargo_home" \
  RUSTUP_HOME="$hostile_rustup_home" \
  RUSTC_WRAPPER="${hostile_tools}/wrapper" \
  RUSTC_WORKSPACE_WRAPPER="${hostile_tools}/wrapper" \
  RUSTFLAGS='--definitely-invalid-rustflag' \
  CARGO_ENCODED_RUSTFLAGS=$'--cfg\x1fdayweave_hostile_encoded_flag' \
  CARGO_BUILD_RUSTC_WRAPPER="${hostile_tools}/wrapper" \
  CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="${hostile_tools}/linker" \
  CODESIGN_ALLOCATE="${hostile_tools}/linker" \
  PERL5LIB="$hostile_tools" \
  PERL5OPT='-MDayWeaveHostile' \
  CARGO_REGISTRIES_CRATES_IO_TOKEN='synthetic-registry-token-must-not-leak' \
  DAYWEAVE_SYNTHETIC_SECRET='synthetic-environment-secret-must-not-leak' \
  "${config_fixture}/scripts/build-macos-scheduler-helper.sh" \
    >"$config_log" 2>&1
config_status=$?
set -e
test "$config_status" -eq 0 \
  || fail 'ambient Rust tools, configuration, or environment affected the build'
test ! -e "$hostile_marker" \
  || fail 'an ambient Rust tool, wrapper, or linker was invoked'
for synthetic_secret in \
  synthetic-credential-must-not-leak \
  synthetic-registry-token-must-not-leak \
  synthetic-environment-secret-must-not-leak
do
  if /usr/bin/grep -Fq -- "$synthetic_secret" "$config_log"; then
    fail 'the isolated build exposed a synthetic secret'
  fi
done
assert_no_private_roots "$config_fixture"

published_helper="${config_fixture}/target/aarch64-apple-darwin/release/dayweave-scheduler-helper"
test -x "$published_helper" && test ! -L "$published_helper" \
  || fail 'the isolated build did not publish an executable helper'
test "$(/usr/bin/stat -f %l "$published_helper")" = 1 \
  || fail 'the isolated build published a hardlinked helper'
test "$(/usr/bin/lipo -archs "$published_helper")" = arm64 \
  || fail 'the isolated build did not publish a thin arm64 helper'
/usr/bin/codesign --verify --strict "$published_helper" >/dev/null 2>&1 \
  || fail 'the isolated build helper failed strict signature verification'
/usr/bin/codesign \
  --verify --strict \
  -R='identifier "com.greengolddog.dayweave.scheduler-helper"' \
  "$published_helper" >/dev/null 2>&1 \
  || fail 'the isolated build helper failed identifier verification'
displayed_requirement="$(
  /usr/bin/codesign --display --requirements - "$published_helper" 2>&1
)"
test "$displayed_requirement" = \
  "Executable=${published_helper}"$'\n''designated => identifier "com.greengolddog.dayweave.scheduler-helper"' \
  || fail 'the isolated build helper has the wrong designated requirement'

previous_helper_identity="$(/usr/bin/stat -f %d:%i "$published_helper")"
PATH="${hostile_tools}:/usr/bin:/bin:/usr/sbin:/sbin" \
  HOME="$hostile_home" \
  CARGO_HOME="$hostile_cargo_home" \
  RUSTUP_HOME="$hostile_rustup_home" \
  RUSTC_WRAPPER="${hostile_tools}/wrapper" \
  RUSTC_WORKSPACE_WRAPPER="${hostile_tools}/wrapper" \
  RUSTFLAGS='--definitely-invalid-rustflag' \
  CARGO_BUILD_RUSTC_WRAPPER="${hostile_tools}/wrapper" \
  CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER="${hostile_tools}/linker" \
  CODESIGN_ALLOCATE="${hostile_tools}/linker" \
  PERL5LIB="$hostile_tools" \
  PERL5OPT='-MDayWeaveHostile' \
  DAYWEAVE_SYNTHETIC_SECRET='synthetic-environment-secret-must-not-leak' \
  "${config_fixture}/scripts/build-macos-scheduler-helper.sh" \
    >"${temporary_root}/replacement.log" 2>&1
test "$(/usr/bin/stat -f %d:%i "$published_helper")" != "$previous_helper_identity" \
  || fail 'the verified helper did not atomically replace the prior output'
test ! -e "$hostile_marker" \
  || fail 'an ambient injection ran during verified output replacement'
assert_no_private_roots "$config_fixture"

printf '%s\n' 'macOS scheduler helper build security regression: PASS'
