# Klipper compatibility — license & provenance record (§23.5)

**Status: DRAFT — awaiting owner approval. The §23.5 gate remains in force:
no Klipper-derived code, tables, tests, or configuration data may land in this
repository until the owner approves this record.**

## Why this document exists

Spec §23.5: "Before copying or deriving code, protocol tables, tests, or
configuration data, record their licenses and provenance." Dryer is Apache-2.0;
Klipper is **GPL-3.0** (all code and in-repo documentation, including the
content served at klipper3d.org, which is generated from the GPL repository).
Mixing the two carelessly would either infect this repo or misappropriate GPL
text. This record states what may be consulted, what may never be copied, and
how each derivation is logged.

## Source inventory

| Source | License | What it is |
|---|---|---|
| github.com/Klipper3d/klipper (code) | GPL-3.0 | host (klippy), MCU firmware, protocol implementation |
| klipper3d.org / repo `docs/` | GPL-3.0 (same repo) | config reference, command reference, protocol notes |
| Community `printer.cfg` files | per-file (often unlicensed) | user configurations |
| Moonraker (Arksine/moonraker) | GPL-3.0 | API server consumed by Mainsail/Fluidd |
| Mainsail / Fluidd | GPL-3.0 / GPL-3.0 | UIs — relevant only as compatibility *clients* |

## Policy (proposed)

**MAY, with a ledger entry per consultation:**
- Read klipper3d.org documentation to understand *behavior and interfaces*
  (config section/option names, command names, semantics). Interface facts —
  names, parameters, value meanings — are used as facts; no documentation prose
  is ever copied or closely paraphrased into this repo.
- Inspect community `printer.cfg` files to understand configuration *structure*.
- Run the real Klipper (unmodified, in a container) as an **execution oracle**:
  Dryer-generated configs are validated by whether actual Klipper accepts them
  in CI. Running GPL software is unrestricted; this is the same pattern dry
  uses with LinuxCNC and pygcode — independent validation without code reuse.

**MUST NOT:**
- Copy or translate any Klipper/Moonraker source code or comments into this repo.
- Copy documentation prose, tables, or examples verbatim.
- Vendor any GPL file, even under `tests/` or `packages/`.

**MUST:**
- Record every consulted source in the ledger below (URL, date, what was derived).
- Keep Phase 1 output (config *generation*) limited to emitting the documented
  config vocabulary from Dryer's own Machine Graph data.

## The hard case: the Klipper MCU protocol (spec Phase 2)

The host↔MCU protocol has **no independent specification** — its only
authoritative definition is GPL source. A clean-room implementation classically
requires a spec author (who reads the source) separate from the implementer
(who reads only the spec). With one owner plus AI agents, that separation is
organizationally thin and should not be claimed casually.

Options for the owner to choose (not needed for Phase 1):

1. **Defer** protocol compatibility; ship Phase 1 (config generation, GPL-free)
   and Phase 3 (native protocol) around it. *Lowest risk; recommended default.*
2. **Document-then-implement with recorded derivation**: write an interface
   spec from the source with full provenance, implement from that spec, and
   accept that the result's independence rests on the "facts vs expression"
   distinction rather than organizational separation. Get legal advice first.
3. **Separate GPL crate/repo**: implement the compat MCU as its own GPL-3.0
   project that *consumes* Dryer's Apache artifacts (lockfiles, resolved
   configs) across a process/serialization boundary. Clean licenses, more repos.

## Consultation ledger

| Date | Source (URL + version/commit) | Consulted for | Derived artifact |
|---|---|---|---|
| — | *(empty — nothing consulted yet)* | | |

## Approval

- [ ] Owner approves the MAY/MUST NOT/MUST policy above
- [ ] Owner selects a Phase-2 protocol option (1/2/3) — may be deferred
- Approved on: ____ · By: ____

Until both the policy box is checked and this file's Status line is changed to
**Approved**, roadmap step 8 stays blocked.
