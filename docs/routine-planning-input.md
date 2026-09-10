# Native fixed-input routine preparation

This checkpoint connects the [authenticated planning witness](routine-planning-witness.md)
to native helper-v2 verification and encrypted custody on macOS and Android.
It is **not completed offline scheduling**. The full
[product requirements](product-requirements.md) and
[discovery answers](discovery-answers.md) remain authoritative.

## Explicit connected action

Settings exposes a private **Prepare** action. It requires healthy encrypted
state, matching device credentials, complete canonical/Habit/occurrence
checkpoints and no unresolved scheduling, execution, provider or authoring
work. Normal sync/recovery must settle existing outboxes and any remote
schedule catch-up latch first; preparation never settles them itself.

The owned operation freezes the original request, complete canonical sources
and process-local state/read/privacy generations. It posts the private
read-only request, validates scope/source/cursor/immutable-input pairing, then
runs the explicit occurrence-aware helper-v2 path. The helper's independently
computed fingerprint, positive-or-empty occurrence head and complete source
accounting must match the qualified response. V1 is unchanged.

Only then does an atomic expected-state transition save the fixed inputs. The
operation rechecks credential binding, cancellation, foreground/private access,
source/read generations, scheduling profile and clock continuity around work
and before custody. A queued action cannot begin after foreground permission
is withdrawn. Changed or late responses cannot overwrite the previous input.

The successful action changes no displayed schedule, publication proof,
occurrence cursor, template, review permission or execution state. It sends no
publication or provider write. `remote_required` retains existing state and
asks for the relevant remote planning/reconciliation path.

## Scope and retained data

The device-auth envelope does not expose workspace/user IDs before this read.
The first qualified response attests those IDs through the owned authenticated
transport; they are stored with the existing origin and stable opaque
credential-binding identifier. Later captures on that binding must match the
retained scope. Neither canonical cache contents nor an opaque hash invents a
pre-existing owner identity.

The encrypted artifact retains the exact original request bytes, lossless
normalized witness schedule/lifecycle, complete canonical helper sources,
scope/binding, captured time and stable input comparisons. Sources include
Inbox and unscheduled descendants. Private notes stay in encrypted canonical
custody and are omitted from the helper's planning projection. No bearer token,
refresh token, reusable review lease or process-local admission is stored in
the artifact.

Server hashes use Rust serialization and remain opaque; native code does not
pretend a locally serialized JSON hash authenticates them. Android additionally
records a domain-separated digest of its exact original request string. macOS
compares its retained original request bytes with the typed request. Both keep
timestamp strings in the normalized schedule; helper-v2 time pairing uses exact
instants rather than relying on a potentially lossy display timestamp.

## Persistence and recovery boundaries

The preparation checkpoint introduced these formats. The separate
[display checkpoint](routine-planning-display.md) extends them without dropping
the saved inputs or existing recovery journals.

- macOS planner schema **29** preserves schema-28 occurrence state and all
  existing journals. The artifact shares the existing ordinary **16 MiB**
  complete-snapshot allowance, with no new publication-only reserve.
- Android Room/payload **25** adds a nullable encrypted artifact and a rollback
  fence without new plaintext data columns. All predecessor formats remain
  readable through their existing migrations; old labels cannot acquire this
  new artifact by injection.
- A capsule itself is bounded to **16 MiB** on both platforms. Android had no
  global full-snapshot ceiling before this checkpoint: **new/replacement capsule
  admission** additionally checks the prospective whole snapshot against
  16 MiB. It does not impose that new total limit on subsequent ordinary saves
  with an unchanged capsule or erase an existing outbox to make room.
- Failed encrypted writes retain the previous durable artifact and recovery
  work. Exact current-state fences detect same-cursor read/privacy changes;
  process-local generations and admissions are never restored as authority.

Restart validates the stored artifact, not the freshness of external services.
Changed sources, profile, ledger, binding, pending work, clock rollback or
planning-day/horizon expiry make reuse ineligible without silently rebuilding
the request with `now()`. Stale artifacts can remain encrypted while recovery
continues. Account/credential quarantine clears the associated private cache
only through the existing lifecycle boundaries.

## Preparation-checkpoint verification

The [shared Rust producer corpus](../fixtures/routine-planning-witness/README.md)
is consumed by both native wire/helper suites. Native tests cover transport
framing and cancellation, complete current-source joins, encrypted migration,
save failures, bounded admission, scope pins and connected prepared-only
control flow. At the preparation checkpoint, root-run verification passed
**44 focused macOS tests** and the full macOS suite with **1,166 executed
tests**, four opt-in skips and no
failures. All macOS application and test sources compiled with warnings denied.
The full Android JVM suite passed **1,759 executed tests**, three opt-in skips
and no failures or errors. Android lint reported zero errors and 34 existing
warnings, none in the new routine-planning files; debug app and test APKs built,
including SQLCipher migration instrumentation source. That instrumentation has
not run on a device. The connected tests use synthetic owned transports and
helper doubles alongside separate exact corpus/adapter tests; this checkpoint
does not claim real Google/owner-account, bundled-JNI/device or full native/service
preparation acceptance. Earlier server/corpus and service-convergence evidence
is recorded in the linked contract. These counts describe preparation, not the
later display checkpoint's verification.

## Implemented display extension and remaining work

The separate [private fixed-input preview](routine-planning-display.md) now
connects the saved capsule to explicit helper-v2 recomputation and an encrypted
**display-only, execution-locked** artifact on both native clients. It retains
the exact validated helper output and exposes only a protected, read-only
presentation. It does not rebuild the original request, advance its clock or
horizon, fall back to v1, or make a new witness request. Source, generation,
privacy, pending-work, clock and durable-state fences surround computation and
storage; restart restores custody but never presentation permission.

The preview does not use the ordinary local installer, which replaces
schedule/publication fields that are part of the saved input comparison.
Canonical schedule, publication proofs and recovery journals remain unchanged;
no existing local-install, execution, publication or Defer guard is removed.
On macOS, local private foreground ownership is granted after the existing
unlock/onboarding gate without depending on a successful execution refresh.
Lock/background withdrawal revokes it, and displaying restored inputs requires
a new explicit recomputation. This ownership is not fresh remote execution or
authentication evidence. The linked display checkpoint records its own
verification and capacity boundaries.

Arbitrary new offline clocks and horizons, source editing, execution credit and
manual-placement policy still need additional representation and authority
handling. Routine cadence, template rebase, nested recurrence, step deferral,
broader process/cross-client recovery, controlled real-service preparation and
convergence, and physical-device acceptance remain open.
