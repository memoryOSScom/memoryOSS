# North-Star Build Plan

This document expands `P10`, `P11`, and `P12` from high-level roadmap phases into a research-backed execution plan.

The goal is not "more memory features." The goal is to turn memoryOSS into:

1. a highly reliable adaptive memory runtime,
2. a portable universal memory layer across clients and providers,
3. an everyday utility product that users keep installed because it removes repeated context work.

## Product Thesis

memoryOSS only becomes a category-defining product if it does all of the following at once:

- stores useful long-term context with low silent-error rates,
- explains why something was remembered or blocked,
- moves memory across apps and devices without losing meaning,
- compiles the right task state instead of dumping raw context,
- proves public utility with repeatable loops, not just one-off demos.

## Non-Negotiable Design Laws

1. Stable lane first, experimental lane second.
   New recall and memory strategies must be shadow-testable against the current stable behavior.
2. Fail closed before fail smart.
   When confidence is low, inject less, ask for more evidence, or abstain.
3. Local-first by default.
   Memory must remain useful offline and under local control.
4. Provenance is part of the product, not internal metadata.
   Every important memory action must be explainable and replayable.
5. A memory runtime needs artifact semantics.
   Export/import is not enough; the product needs portable memory bundles with versioning and integrity.
6. Public proof must track product ambition.
   Claims need published benchmark loops, not only private intuition.
7. Utility beats novelty.
   A moonshot feature that is hard to trust or hard to operate is not yet product leverage.

## Research Base

The plan below uses the following external references as anchor points:

- `GrepRAG` - lightweight lexical retrieval, identifier-weighted reranking, structure-aware deduplication.
  https://arxiv.org/abs/2601.23254
- `RepoCoder` - iterative retrieve -> generate loops for repo-level completion.
  https://arxiv.org/abs/2303.12570
- `Repoformer` - selective retrieval, avoid retrieval when unnecessary.
  https://arxiv.org/abs/2403.10059
- `MemGPT` - hierarchical memory tiers and interrupt-style control.
  https://arxiv.org/abs/2310.08560
- `Self-RAG` - retrieve and critique only when useful.
  https://arxiv.org/abs/2310.11511
- `CRAG` - lightweight retrieval evaluator and corrective fallback.
  https://arxiv.org/abs/2401.15884
- `Adaptive-RAG` - route by query complexity instead of fixed retrieval policy.
  https://arxiv.org/abs/2403.14403
- `LongRAG` - longer retrieval units and stronger reader/retriever balance.
  https://arxiv.org/abs/2406.15319
- `MemoRAG` - global memory plus clue-guided retrieval.
  https://arxiv.org/abs/2409.05591
- `MacRAG` - multi-scale context construction.
  https://arxiv.org/abs/2505.06569
- `Recursive Language Models` - recursive inspection and decomposition over arbitrarily long prompts.
  https://arxiv.org/abs/2512.24601
- `Toolformer` - small specialized tool-use decisions can be taught instead of delegated to a giant model every time.
  https://arxiv.org/abs/2302.04761
- `Local-first software` - user control, offline utility, multi-device sync, and conflict-aware local ownership.
  https://martin.kleppmann.com/2019/10/23/local-first-at-onward.html

The plan also considers current product reality in:

- OpenAI Memory FAQ
  https://help.openai.com/en/articles/8590148-memory-in-chatgpt
- Claude Projects and project knowledge
  https://support.claude.com/en/articles/9517075-what-are-projects
- Cursor Memories and Rules
  https://docs.cursor.com/context/memories
  https://docs.cursor.com/context/rules
- LangMem conceptual guide
  https://langchain-ai.github.io/langmem/concepts/conceptual_guide/

## Phase P10 - v1.6 Adaptive Recall and Operator UX

### Phase Objective

Make memoryOSS operable, measurable, and fail-closed before taking larger architecture bets.

### P10-T1 Add doctor, status, and recent

- Thesis:
  A memory system cannot be trusted if operators cannot tell whether it is healthy.
- Research basis:
  LangMem distinguishes semantic, episodic, and procedural memory and treats memory operations as explicit system concepts, not hidden implementation detail.
- Build shape:
  Add `memoryoss status`, `memoryoss doctor`, and `memoryoss recent`, plus matching admin endpoints.
- Hard gates:
  Must work without any external provider.
  Must report namespace counts, worker health, recall health, extraction health, and auth/config issues.
- Kill criteria:
  If status output becomes a dump of internals instead of an operator view, redesign before shipping.

### P10-T2 Build candidate review inbox

- Thesis:
  Aggressive extraction only becomes safe when humans can review proposed memory changes quickly.
- Research basis:
  Cursor memories use a sidecar plus approval flow; LangMem also frames memory writing as an explicit action.
- Build shape:
  Create a queue for candidate, contested, and rejected memories with confirm/reject/supersede actions.
- Hard gates:
  Review actions must produce auditable provenance.
  Queue summaries must be visible without exposing raw internal rows everywhere.
- Kill criteria:
  If review takes longer than direct CLI/API edits, the product path is wrong.

### P10-T3 Expand experimental evaluation harness

- Thesis:
  The current proof set is strong but too narrow for moonshot retrieval work.
- Research basis:
  Self-RAG, CRAG, and Adaptive-RAG all imply policy choice; policy choice without benchmark coverage is guesswork.
- Build shape:
  Expand extraction eval to 100+ cases, add retrieval/injection/abstain benchmark, add shadow-mode comparison.
- Hard gates:
  Stable and experimental metrics must be reported separately.
  Wrong-injection and abstain metrics become first-class report outputs.
- Kill criteria:
  If new experiments cannot be measured on the same fixed sets, they do not enter the runtime.

### P10-T4 Add retrieval confidence gate

- Thesis:
  The runtime should choose between inject, abstain, and need-more-evidence, not blindly inject.
- Research basis:
  Self-RAG, CRAG, Adaptive-RAG.
- Build shape:
  Add a lightweight scoring gate ahead of injection.
- Hard gates:
  Default behavior must be fail-closed.
  p95 proxy latency budget: +150ms max.
- Kill criteria:
  If wrong-injection falls but missed-evidence explodes, the gate needs redesign.

### P10-T5 Route by identifiers first

- Thesis:
  Code- and task-heavy recall should route differently from vague semantic recall.
- Research basis:
  GrepRAG, RepoCoder, Repoformer.
- Build shape:
  Add identifier-aware routing, exact/FTS-first branch, identifier-weighted reranking, structural dedup.
- Hard gates:
  Must outperform the stable lane on identifier-heavy evals without raising false positives.
- Kill criteria:
  If this branch helps code queries but hurts policy or preference recall, keep it query-class-specific.

### P10-T6 Add summary plus evidence recall

- Thesis:
  Raw fact injection is too flat. Operators and agents need summary plus drill-down evidence.
- Research basis:
  LongRAG, MemoRAG, MacRAG.
- Build shape:
  Emit a concise summary layer with attached evidence/provenance layer.
- Hard gates:
  Every summary must be expandable into explicit supporting evidence.
- Kill criteria:
  If summary generation becomes another hallucination surface, keep evidence-first mode available.

### P10-T7 Add recursive recall engine

- Thesis:
  Some tasks are too large for one-pass recall and need recursive inspect/refine loops.
- Research basis:
  Recursive Language Models.
- Build shape:
  Feature-flagged recursive retrieval with hard depth, token, and time budgets.
- Hard gates:
  Disabled by default.
  Automatic fallback to stable lane on timeout or budget exhaustion.
- Kill criteria:
  If recursive recall does not beat the stable lane on defined hard tasks, keep it experimental only.

### P10-T8 Add working-set tiers

- Thesis:
  Not all memories play the same role; the runtime should make this visible.
- Research basis:
  MemGPT, LangMem.
- Build shape:
  Distinguish working set, candidate queue, evidence store, archive.
- Hard gates:
  Recall, review, decay, and consolidation must all respect tier semantics.
- Kill criteria:
  If tiers become only labels with no behavior difference, the model is too weak.

### P10-T9 Sync roadmap and proof surfaces

- Thesis:
  Trust is damaged by roadmap drift faster than by missing glamour features.
- Research basis:
  Product discipline, not paper novelty.
- Build shape:
  Align README, roadmap, tests page, whitepaper, and PRD.
- Hard gates:
  Stable and experimental claims must be visibly separated.
- Kill criteria:
  None; this is maintenance debt and should not be skipped.

### P10-T10 Restore Windows vector parity

- Thesis:
  Platform support without performance honesty is fake support.
- Research basis:
  Engineering parity, not literature.
- Build shape:
  Improve the Windows retrieval backend or bound its limitations explicitly.
- Hard gates:
  Windows path must be covered in release-smoke and benchmark suites.
- Kill criteria:
  If parity cannot be reached safely, document the hard limits and keep scope narrow.

### P10 Exit Criteria

- Operators can see and control memory behavior.
- Retrieval policy can abstain.
- Stable lane and experimental lane share one evaluation harness.
- Public proof surfaces reflect reality.

## Phase P11 - v2.0 Universal Memory Runtime

### Phase Objective

Move from "shared local memory layer" to "portable memory runtime."

### P11-T1 Define universal memory contract

- Thesis:
  A runtime needs stable semantics, not only working endpoints.
- Research basis:
  LangMem memory types; MemGPT memory tiers; current market products still expose product-specific memory semantics.
- Build shape:
  Write a versioned contract for memory objects, scopes, provenance, merge, supersede, replay, branch.
- Hard gates:
  Stable runtime semantics must be separated from experimental retrieval strategies.
- Kill criteria:
  If the contract only mirrors current implementation details, it is not a real runtime contract.

### P11-T2 Build memory passport bundles

- Thesis:
  Backup/restore is not enough; users need portable selective memory bundles.
- Research basis:
  Local-first software principles plus the lack of cross-tool portability in current vendor memory products.
- Build shape:
  Define portable, signable passport bundles for personal, project, and team memory scopes.
- Hard gates:
  Must support dry-run import, conflict preview, and roundtrip fidelity checks.
- Kill criteria:
  If bundles are just full dumps in disguise, the feature is incomplete.

### P11-T3 Add cross-app memory adapters

- Thesis:
  memoryOSS becomes default only when it absorbs existing memory islands.
- Research basis:
  OpenAI Memory, Claude Projects, Cursor Memories all keep memory in product-specific silos.
- Build shape:
  Build import/ingest bridges into the runtime contract.
- Hard gates:
  At least three real adapter paths.
  Provenance must survive import.
- Kill criteria:
  If adapters become brittle scrapers with no semantic mapping, the contract is not ready.

### P11-T4 Build memory time machine

- Thesis:
  People will trust memory when they can inspect, replay, undo, and branch it.
- Research basis:
  MemGPT interrupts; internal replay-manifest/proof-governance ideas suggest replayability is a real moat.
- Build shape:
  Add history, branch-from-here, replay, and safe undo surfaces.
- Hard gates:
  Replay must reproduce visible state on a clean instance.
  Namespace and privacy boundaries must remain intact.
- Kill criteria:
  If replay fidelity is weak, do not market history as deterministic.

### P11-T5 Add policy memory firewall

- Thesis:
  Memory should not only help actions; it should prevent bad actions.
- Research basis:
  Self-RAG and CRAG motivate gating; here the gate applies to action classes instead of only retrieval.
- Build shape:
  Encode deployment, security, and preference policies as active preflight guards.
- Hard gates:
  Explain must name the blocking or warning policy.
  Need false-block benchmarks.
- Kill criteria:
  If false-block rate is operationally painful, keep warn-only mode as default.

### P11-T6 Build ambient memory sidecar

- Thesis:
  A universal runtime eventually needs to observe relevant work passively.
- Research basis:
  Cursor sidecar model and approval loop.
- Build shape:
  Add local observers for selected sources and send candidates into the review inbox.
- Hard gates:
  Privacy-preserving defaults and explicit provenance are mandatory.
- Kill criteria:
  If sidecar capture generates too much noise, narrow the source list before expanding.

### P11-T7 Compile task state

- Thesis:
  The runtime moat is not retrieval alone; it is compiling the smallest sufficient working state.
- Research basis:
  Recursive Language Models, MemoRAG, LongRAG, MacRAG.
- Build shape:
  For each task class, compile facts, constraints, recent actions, open questions, and evidence into an explicit task state.
- Hard gates:
  Benchmark must show better or equal outcome quality at lower prompt footprint.
- Kill criteria:
  If compiled state hides uncertainty, it is worse than flat recall.

### P11-T8 Prove universal memory loop

- Thesis:
  "Write once, remember everywhere" must be publicly testable.
- Research basis:
  Product proof, not paper citation.
- Build shape:
  Public benchmark showing create -> review -> export -> import -> replay -> reuse across different client types.
- Hard gates:
  Publish portability success rate, merge-conflict rate, replay fidelity, task-state quality.
- Kill criteria:
  If the loop only works in a heavily scripted happy path, do not make universal claims.

### P11-T9 Publish runtime conformance kit

- Thesis:
  A default runtime needs a real conformance target, not only a specification document.
- Research basis:
  Mature formats and protocols only become ecosystem standards when they ship fixtures, schemas, and compatibility suites.
- Build shape:
  Publish schemas, canonical fixtures, reference readers/writers, and an automated compatibility harness.
- Hard gates:
  Rust, Python, and TypeScript reference paths must pass the same fixture sets.
  Versioning and deprecation rules must be explicit.
- Kill criteria:
  If only memoryOSS itself can pass the suite, the runtime is still effectively proprietary.

### P11 Exit Criteria

- The runtime has portable semantics.
- Memory can be transported as a selective bundle.
- History and replay are real features, not marketing.
- Cross-app portability is publicly proven on a bounded benchmark.
- External implementations have a real conformance target.

## Phase P12 - v3.0 Ubiquitous Memory Utility

### Phase Objective

Turn the runtime into an everyday utility product with artifact, sync, visibility, and habit-forming loops.

### Must-Build-First Block

These are the highest-leverage utility bets:

- `P12-T1` memory bundle format
- `P12-T2` multi-device sync fabric
- `P12-T3` universal memory HUD
- `P12-T9` universal memory reader
- `P12-T10` trust and revocation fabric
- `P12-T11` zero-friction update plane
- `P12-T12` compatibility and LTS guarantees
- `P12-T8` everyday utility proof loop

If this block does not work, the rest of P12 should be treated as optional research rather than category-building work.

### P12-T1 Ship memory bundle format

- Thesis:
  Utility products win by owning a standard artifact, not just a daemon.
- Research basis:
  Local-first software; portable runtime semantics from P11.
- Build shape:
  Define versioned bundle format plus URI/attachment semantics.
- Hard gates:
  Preview, diff, validation, signing, and forward-compatibility testing.
- Kill criteria:
  If bundles cannot be safely inspected without import, they are too dangerous to share.

### P12-T2 Build multi-device sync fabric

- Thesis:
  A local utility becomes a daily utility only when it follows the user across devices.
- Research basis:
  Local-first software and CRDT-style sync principles.
- Build shape:
  Selective sync with local-first default, optional end-to-end encryption, branch/conflict preservation.
- Hard gates:
  Offline resume and convergence tests.
  Soak tests for conflict handling.
- Kill criteria:
  If sync requires central always-on authority for correctness, the local-first promise has been violated.

### P12-T3 Build universal memory HUD

- Thesis:
  A utility must always be one shortcut away.
- Research basis:
  Product pattern, not arXiv novelty.
- Build shape:
  A desktop/TUI launcher for search, why, recent, review, import/export, and policy blocks.
- Hard gates:
  Must be faster than opening a client-specific settings panel.
- Kill criteria:
  If HUD is slower than CLI for power users and slower than app-native memory UIs for regular users, redesign.

### P12-T4 Expand ambient connector mesh

- Thesis:
  Once the HUD and sidecar exist, the product should learn from real workstreams.
- Research basis:
  Cursor sidecar pattern, ambient memory thesis from P11.
- Build shape:
  Connect editor, terminal, browser/docs, tickets, calendar/incident sources under one provenance model.
- Hard gates:
  Opt-in only; common redaction/privacy defaults.
- Kill criteria:
  If connector noise overwhelms review capacity, halt expansion and improve ranking first.

### P12-T5 Induce memory primitive algebra

- Thesis:
  The deeper moat is not better retrieval alone but better memory representation.
- Research basis:
  This is the most research-heavy task in the roadmap. It also aligns with adjacent internal work on primitive induction and operator transfer.
- Build shape:
  Add experimental decomposition into primitives such as policy, constraint, incident, habit, environment, actor, evidence, and task-state operators.
- Hard gates:
  Primitive-based recall or merge must beat flat-fact baselines on fixed evals.
- Kill criteria:
  If the algebra is elegant but does not improve measured task outcomes, keep it research-only.

### P12-T6 Distill memory coprocessor

- Thesis:
  A default utility should not need a frontier model for every small memory decision.
- Research basis:
  Toolformer suggests specialized tool decisions can be learned; distillation work suggests fixed-cost small models can replace repeated large-model calls on narrow tasks.
- Build shape:
  Distill a local memory specialist for gate/classify/merge/redact/policy-check/compile subtasks.
- Hard gates:
  Must be benchmarked head-to-head against the larger model path on quality, latency, and privacy-sensitive flows.
- Kill criteria:
  If quality falls or confidence is opaque, confine the coprocessor to advisory mode.

### P12-T7 Add team memory governance

- Thesis:
  Team memory without governance will become stale, contradictory, and untrusted.
- Research basis:
  Mature collaboration systems win because they encode review and ownership, not because they only sync bytes.
- Build shape:
  Branches, owners, approvals, watchlists, scoped review-required merges.
- Hard gates:
  Governance must integrate with provenance, policy, replay, and review.
- Kill criteria:
  If governance becomes bureaucracy without measurable error reduction, simplify aggressively.

### P12-T8 Prove everyday utility loop

- Thesis:
  A true utility product proves repeated daily benefit across devices, clients, and teams.
- Research basis:
  Utility products survive because people feel the loss when they remove them.
- Build shape:
  Publish repeatable benchmark loops for repeated-context elimination, portability, replay fidelity, blocked bad actions, and review throughput.
- Hard gates:
  Must include at least three realistic loops over multiple clients and devices.
- Kill criteria:
  If public proof depends on custom environment hand-holding, the product is not yet utility-grade.

### P12-T9 Ship universal memory reader

- Thesis:
  A portable artifact only becomes category-defining when people can safely open it everywhere.
- Research basis:
  Reader-first product patterns from Acrobat Reader and archive utilities.
- Build shape:
  Ship a read-only reader for bundles with preview, provenance, signature, scope, and diff views.
- Hard gates:
  Must work without starting the full runtime stack.
  Must safely inspect malformed or hostile bundles.
- Kill criteria:
  If preview requires implicit import or full daemon startup, it is not a true reader.

### P12-T10 Build trust and revocation fabric

- Thesis:
  Portable memory is dangerous without identity, signing, and revocation semantics.
- Research basis:
  Trust fabrics from package ecosystems, secure collaboration systems, and signing chains.
- Build shape:
  Add authorship keys, device identities, signing chains, key rotation, and revocation for bundles, passports, and sync peers.
- Hard gates:
  Trust status must be visible in the reader and HUD.
  Revocation must propagate without silent ambiguity.
- Kill criteria:
  If compromise handling requires manual archaeology, trust semantics are too weak.

### P12-T11 Ship zero-friction update plane

- Thesis:
  A utility product stays installed because updates are boring, safe, and reversible.
- Research basis:
  Mature desktop/runtime utilities win with frictionless install, upgrade, and rollback.
- Build shape:
  Signed installers, background upgrades, rollback, and config/schema migration handling.
- Hard gates:
  One known-bad update path must be recoverable in automated tests.
  Stable/beta/rollback channels must be explicit.
- Kill criteria:
  If upgrades scare operators into skipping releases, utility adoption stalls.

### P12-T12 Guarantee compatibility and LTS

- Thesis:
  Portable memory only becomes trustworthy when users believe their artifacts survive years, not weeks.
- Research basis:
  Standard file formats and runtimes earn trust through compatibility guarantees and deprecation discipline.
- Build shape:
  Add support windows, compatibility matrix, fixture-based old-version tests, and clear deprecation rules.
- Hard gates:
  N, N-1, and N-2 compatibility tests for bundle/passport/runtime core paths.
- Kill criteria:
  If each release can strand old bundles or sync peers, the utility claim collapses.

### P12 Exit Criteria

- memoryOSS has a real portable artifact format.
- It syncs in a local-first way.
- It has a universal visible surface.
- It proves daily utility, not just technical possibility.
- Bundles can be safely read anywhere.
- Trust, revocation, updates, and compatibility are boring and dependable.

## Cross-Phase Execution Order

### Block 1 - Reliability and Operator Control

Do first:

- `P10-T1`
- `P10-T2`
- `P10-T3`
- `P10-T4`
- `P10-T5`

Reason:
This block turns the current engine into something measurable, debuggable, and adaptable.

### Block 2 - Runtime Semantics

Do next:

- `P10-T6`
- `P10-T8`
- `P11-T1`
- `P11-T2`
- `P11-T4`
- `P11-T7`

Reason:
This block creates portable semantics, explicit state, and task compilation.

### Block 3 - Portability and Cross-App Proof

Do next:

- `P11-T3`
- `P11-T8`
- `P11-T9`
- `P12-T1`
- `P12-T2`

Reason:
This is where memoryOSS can first make a credible "write once, remember everywhere" claim.

### Block 4 - Utility Surface

Do next:

- `P12-T3`
- `P12-T9`
- `P12-T10`
- `P12-T11`
- `P12-T12`
- `P12-T8`

Reason:
This is where the product becomes visible, trusted, and habit-forming.

### Block 5 - High-Risk Research Bets

Only after earlier blocks show clear wins:

- `P10-T7`
- `P11-T5`
- `P11-T6`
- `P12-T4`
- `P12-T5`
- `P12-T6`
- `P12-T7`

Reason:
These tasks can create a serious moat, but they also carry the highest complexity, privacy, and adoption risk.

## Kill-Switch Rules

Any experimental path should be halted or kept experimental if one of these is true:

- wrong-injection rate increases materially with no compensating outcome gain,
- explainability gets worse,
- operators lose control over review or rollback,
- the benchmark gain exists only on one narrow demo,
- multi-device or cross-app portability becomes brittle,
- the product story becomes harder to explain than the product value it creates.

## What "Perfect" Means Here

This plan does not assume a single miracle algorithm.

It assumes that a category-defining memory product is built by combining:

- strong retrieval and gating,
- explicit memory semantics,
- portable artifacts,
- local-first sync,
- visible control surfaces,
- public proof loops,
- and only then the deep research moat around task-state compilation, primitive algebra, and small local coprocessors.

If memoryOSS executes the first half well, it becomes credible.
If it executes the second half well, it becomes hard to replace.

## Reality Check

Categories:

- `Must` = required for the core product claim and moat.
- `Conditional` = build only if earlier bets show strong signal.
- `Later` = promising but too risky or too secondary for the current critical path.
- `Cut as standalone` = do not treat as its own strategic pillar; fold into ongoing hygiene or maintenance.

### P10 Classification

- `P10-T1 doctor/status/recent` -> `Must`
  Reason: without operator visibility, the runtime will not be trusted.
- `P10-T2 candidate review inbox` -> `Must`
  Reason: aggressive memory writing without review control is not shippable.
- `P10-T3 expanded experimental evaluation harness` -> `Must`
  Reason: every later bet depends on this shared measurement layer.
- `P10-T4 retrieval confidence gate` -> `Must`
  Reason: fail-closed retrieval is core to product trust.
- `P10-T5 route by identifiers first` -> `Must`
  Reason: this is one of the clearest practical quality gains and likely part of the moat.
- `P10-T6 summary plus evidence recall` -> `Must`
  Reason: explainable memory needs more than flat fact injection.
- `P10-T7 recursive recall engine` -> `Later`
  Reason: high upside, but not needed to prove the runtime thesis and too easy to overbuild.
- `P10-T8 working-set tiers` -> `Conditional`
  Reason: useful if summary/evidence and task-state compilation win, otherwise premature structure.
- `P10-T9 sync roadmap and proof surfaces` -> `Cut as standalone`
  Reason: necessary hygiene, but not a strategic roadmap pillar. Keep as ongoing maintenance.
- `P10-T10 restore Windows vector parity` -> `Later`
  Reason: important product quality, but not part of the first universal-runtime moat.

### P11 Classification

- `P11-T1 universal memory contract` -> `Must`
  Reason: this is the foundation of the whole runtime story.
- `P11-T2 memory passport bundles` -> `Must`
  Reason: no portability, no universal memory runtime.
- `P11-T3 cross-app memory adapters` -> `Must`
  Reason: default runtime status requires absorbing memory islands, not just serving a new one.
- `P11-T4 memory time machine` -> `Must`
  Reason: this is core to the "Git for AI memory" claim.
- `P11-T5 policy memory firewall` -> `Conditional`
  Reason: strategically strong, but the runtime can still win before the firewall becomes central.
- `P11-T6 ambient memory sidecar` -> `Conditional`
  Reason: very attractive, but depends on review, trust, and noise controls being solid first.
- `P11-T7 task-state compiler` -> `Must`
  Reason: this is the biggest conceptual leap beyond ordinary memory/RAG products.
- `P11-T8 prove universal memory loop` -> `Must`
  Reason: without public proof, P11 remains architecture, not product.
- `P11-T9 runtime conformance kit` -> `Must`
  Reason: a real runtime needs executable compatibility, not just docs.

### P12 Classification

- `P12-T1 memory bundle format` -> `Must`
  Reason: this is the file-format/artifact moat.
- `P12-T2 multi-device sync fabric` -> `Must`
  Reason: a local tool becomes an everyday utility only when it follows the user.
- `P12-T3 universal memory HUD` -> `Must`
  Reason: utilities must be visible and fast to access.
- `P12-T4 ambient connector mesh` -> `Conditional`
  Reason: strong adoption driver, but only after sidecar/review/noise handling prove out.
- `P12-T5 memory primitive algebra` -> `Later`
  Reason: potentially a huge moat, but too research-heavy for the first utility product win.
- `P12-T6 distilled memory coprocessor` -> `Later`
  Reason: valuable if economics/privacy become pressing, but not needed to prove category fit.
- `P12-T7 team memory governance` -> `Conditional`
  Reason: high-value if the product wins in teams; avoid enterprise bureaucracy too early.
- `P12-T8 everyday utility loop` -> `Must`
  Reason: this is the benchmark proof that the product belongs on every machine.
- `P12-T9 universal memory reader` -> `Must`
  Reason: Acrobat/WinRAR-level products require reading/previewing before full runtime adoption.
- `P12-T10 trust and revocation fabric` -> `Must`
  Reason: portable memory without trust semantics is too dangerous to become standard.
- `P12-T11 zero-friction update plane` -> `Must`
  Reason: utilities stay installed because updates are boring and reversible.
- `P12-T12 compatibility and LTS` -> `Must`
  Reason: long-term trust in artifacts and sync requires explicit compatibility discipline.

### What To Actually Cut

Only two current tasks should be cut as standalone roadmap pillars:

- `P10-T9` should become ongoing release/documentation hygiene, not a headline roadmap task.
- `P10-T10` should not drive strategic sequencing; keep it in a platform-quality lane and revisit once the runtime core is proven.

Everything else should remain in the plan, but not at equal priority.

## Master Sequence

This is the exact implementation order to execute from the current state.

### Wave 0 - Execution Baseline

Purpose:
Keep the project honest while the roadmap gets ambitious.

Tasks:

- Treat `P10-T9` as ongoing hygiene, not as a roadmap wave.
- Keep `P10-T10` in a side platform-quality lane only when it does not slow the core runtime path.

Stop/go gate:

- The roadmap, proof pages, and local plan stay aligned enough that the team does not lose trust in its own sequence.

### Wave 1 - Measurement First

Purpose:
Build the benchmark substrate before changing behavior.

Primary task:

- `P10-T3` expanded experimental evaluation harness

Why first:

- Every later bet depends on stable-vs-experimental comparisons.
- Without this wave, later improvements are mostly anecdotal.

Exit gate:

- 100+ extraction cases exist.
- Retrieval/injection/abstain eval exists.
- Stable-vs-experimental reporting is visible in the report pipeline.

### Wave 2 - Operator Control

Purpose:
Make the system operable before making it smarter.

Tasks:

- `P10-T1` doctor/status/recent
- `P10-T2` candidate review inbox

Why now:

- These tasks give the operator control surfaces for everything that follows.

Exit gate:

- An operator can inspect health, recent actions, and pending candidates without raw database archaeology.

### Wave 3 - Adaptive Recall Core

Purpose:
Improve retrieval quality in the most practical, low-regret ways.

Tasks:

- `P10-T4` retrieval confidence gate
- `P10-T5` route by identifiers first
- `P10-T6` summary plus evidence recall

Why now:

- This is the strongest near-term quality gain and produces the first truly differentiated runtime behavior.

Exit gate:

- Wrong-injection rate is lower than the stable baseline.
- Identifier-heavy tasks beat the prior path.
- Summary + evidence is explainable and benchmarked.

### Wave 4 - Runtime Semantics Core

Purpose:
Turn memoryOSS from a featureful system into a true runtime.

Tasks:

- `P11-T1` universal memory contract
- `P11-T2` memory passport bundles
- `P11-T4` memory time machine
- `P11-T9` runtime conformance kit

Why now:

- This wave defines the actual object model, portability, replayability, and standardizability.

Exit gate:

- The runtime contract is versioned.
- Portable bundles exist.
- Replay/branch/history are real.
- A conformance harness exists for multiple language paths.

### Wave 5 - Compiled Memory and Cross-App Proof

Purpose:
Prove that the runtime is more than storage and more than one app.

Tasks:

- `P11-T7` task-state compiler
- `P11-T3` cross-app memory adapters
- `P11-T8` prove universal memory loop

Why now:

- This is the first moment where memoryOSS can credibly claim "write once, remember everywhere."

Exit gate:

- At least one compiled task-state path beats flat memory injection.
- At least one cross-app loop works end-to-end and is publicly benchmarked.

### Wave 6 - Portable Artifact Utility Core

Purpose:
Build the non-negotiable prerequisites for an Acrobat/WinRAR-class utility.

Tasks:

- `P12-T1` memory bundle format
- `P12-T9` universal memory reader
- `P12-T10` trust and revocation fabric
- `P12-T11` zero-friction update plane
- `P12-T12` compatibility and LTS

Why now:

- This is the utility trust layer: readable artifacts, trust semantics, boring updates, long-term compatibility.

Exit gate:

- Memory bundles are a real artifact.
- They can be opened safely without import.
- Trust/revocation is visible.
- Updates and compatibility are operationally boring.

### Wave 7 - Everyday Utility Layer

Purpose:
Make the product something people want installed all the time.

Tasks:

- `P12-T2` multi-device sync fabric
- `P12-T3` universal memory HUD
- `P12-T8` prove everyday utility loop

Why now:

- This wave turns portability into daily habit and visible utility.

Exit gate:

- The product syncs across devices.
- The HUD makes the runtime easy to access.
- Public utility loops show real repeated-context savings and workflow relief.

### Wave 8 - Conditional Expansion

Purpose:
Only extend into broader adoption surfaces if the core runtime and utility waves already win.

Tasks:

- `P10-T8` working-set tiers
- `P11-T5` policy memory firewall
- `P11-T6` ambient memory sidecar
- `P12-T4` ambient connector mesh
- `P12-T7` team memory governance

Why conditional:

- These are strong multipliers, but only after the core runtime is trusted and useful.

Exit gate:

- Review capacity, trust semantics, and product utility remain strong after adding these flows.

### Wave 9 - Moonshot Research Lane

Purpose:
Pursue the deepest moat only after the product has already become credible and useful.

Tasks:

- `P10-T7` recursive recall engine
- `P12-T5` memory primitive algebra
- `P12-T6` distilled memory coprocessor

Why later:

- These can be category-defining, but they are the easiest place to burn time before product-market clarity.

Exit gate:

- Each moonshot must beat the prior path on fixed public benchmarks or remain experimental.

## First Build Step

If execution starts now, the first task should be:

- `P10-T3` expanded experimental evaluation harness

Immediate follow-on after that:

- `P10-T1`
- `P10-T2`
- `P10-T4`
- `P10-T5`
- `P10-T6`

Reason:

- This sequence creates measurement first, then operator control, then adaptive quality gains.

## Origin Map

Labels:

- `Borrowed` = directly inspired by an external paper, product, or established systems pattern.
- `Synthesized` = assembled from multiple sources plus current memoryOSS realities.
- `Novel bet` = the main proprietary or category-defining wager in this plan.

### P10

- `P10-T1 doctor/status/recent`
  Borrowed: operator dashboards and health surfaces from mature infrastructure tools.
  Synthesized: adapts those ideas to memory-specific lifecycle, recall, worker, and auth health.
  Novel bet: treating day-2 memory operations as a first-class product surface rather than hidden admin plumbing.
- `P10-T2 candidate review inbox`
  Borrowed: approval loops in tools like Cursor and human-in-the-loop memory writing.
  Synthesized: merges contradiction, lifecycle, provenance, and review into one queue.
  Novel bet: making memory review the central safety valve for all future aggressive memory automation.
- `P10-T3 expanded experimental evaluation harness`
  Borrowed: benchmark-first culture from retrieval and agent papers.
  Synthesized: one shared harness for extraction, retrieval, injection, abstain, and stable-vs-experimental comparison.
  Novel bet: making shadow-mode memory evaluation a permanent product discipline, not a temporary research tool.
- `P10-T4 retrieval confidence gate`
  Borrowed: Self-RAG, CRAG, Adaptive-RAG.
  Synthesized: inject / abstain / need-more-evidence as a runtime decision inside memoryOSS.
  Novel bet: memoryOSS as an explicit fail-closed retrieval governor.
- `P10-T5 route by identifiers first`
  Borrowed: GrepRAG, RepoCoder, Repoformer.
  Synthesized: lexical routing, identifier-weighted reranking, and structural dedup inside a general memory runtime.
  Novel bet: applying repo-style retrieval discipline to broad operational memory, not only code completion.
- `P10-T6 summary plus evidence recall`
  Borrowed: LongRAG, MemoRAG, MacRAG.
  Synthesized: summary/evidence split plus explain/drill-down operator surfaces.
  Novel bet: turning every recall result into a compact but inspectable memory object.
- `P10-T7 recursive recall engine`
  Borrowed: Recursive Language Models.
  Synthesized: budgeted recursive retrieval behind explicit flags and stable-lane fallback.
  Novel bet: recursive memory retrieval as a bounded runtime capability rather than a paper-only result.
- `P10-T8 working-set tiers`
  Borrowed: MemGPT and memory-tier systems patterns.
  Synthesized: candidate, working set, evidence, archive bound to actual review/decay/consolidation behavior.
  Novel bet: visible memory-role semantics as part of the product contract.
- `P10-T9 sync roadmap and proof surfaces`
  Borrowed: none in a research sense; this is execution discipline.
  Synthesized: binds roadmap, tests, README, whitepaper, and stable/experimental claims.
  Novel bet: none; this is trust hygiene.
- `P10-T10 restore Windows vector parity`
  Borrowed: none; this is platform engineering.
  Synthesized: platform parity plus release-smoke benchmarking.
  Novel bet: none; this is product maturity work.

### P11

- `P11-T1 universal memory contract`
  Borrowed: memory type distinctions from LangMem and tiered-memory thinking from MemGPT.
  Synthesized: stable object semantics for user, team, project, evidence, policy, provenance, merge, supersede, branch, replay.
  Novel bet: framing memoryOSS as a true runtime contract rather than a proxy-plus-database.
- `P11-T2 memory passport bundles`
  Borrowed: local-first portability, signed artifacts, export/import discipline.
  Synthesized: selective identity/project/team memory bundles with dry-run merge previews.
  Novel bet: portable AI-memory passports as a product object people can carry between tools.
- `P11-T3 cross-app memory adapters`
  Borrowed: the observation that current products silo memory by app.
  Synthesized: adapters that normalize foreign memory islands into one runtime contract.
  Novel bet: using memoryOSS as the convergence layer for otherwise incompatible AI-memory systems.
- `P11-T4 memory time machine`
  Borrowed: Git-style history, replay thinking, and provenance-heavy system design.
  Synthesized: memory replay, branch-from-here, undo, contradiction history, and final-state reproduction.
  Novel bet: treating memory itself like a version-controlled operational artifact.
- `P11-T5 policy memory firewall`
  Borrowed: gating logic from retrieval papers and guardrail concepts from secure systems.
  Synthesized: memories that actively warn, block, or demand confirmation before risky actions.
  Novel bet: memory as an action firewall, not just context.
- `P11-T6 ambient memory sidecar`
  Borrowed: sidecar capture patterns and passive signal collection.
  Synthesized: local sources feed reviewable candidate memories with explicit provenance.
  Novel bet: ambient-but-reviewable memory accumulation as the default path.
- `P11-T7 task-state compiler`
  Borrowed: RLM, multi-scale retrieval, and state abstraction ideas.
  Synthesized: compile facts, constraints, evidence, recent actions, and open questions into a minimal working state.
  Novel bet: the core shift from retrieval engine to memory runtime compiler.
- `P11-T8 prove universal memory loop`
  Borrowed: benchmark/public-proof culture.
  Synthesized: create -> review -> export -> import -> replay -> reuse across heterogeneous clients.
  Novel bet: making portability itself a first-class benchmark, not only an implementation detail.
- `P11-T9 runtime conformance kit`
  Borrowed: protocol/file-format conformance culture.
  Synthesized: schemas, fixtures, compatibility harness, and reference readers/writers for the memory runtime.
  Novel bet: treating AI-memory semantics like a standard with executable compatibility tests.

### P12

- `P12-T1 memory bundle format`
  Borrowed: file/archive/portable artifact product patterns.
  Synthesized: memory-specific signed bundle format plus URI/attachment semantics.
  Novel bet: a portable AI-state artifact that could become the standard interchange format for memory.
- `P12-T2 multi-device sync fabric`
  Borrowed: local-first software and conflict-aware sync systems.
  Synthesized: selective encrypted sync that preserves replay, branch, and provenance semantics.
  Novel bet: syncing memory semantics, not just database rows.
- `P12-T3 universal memory HUD`
  Borrowed: Spotlight/launcher/HUD product patterns.
  Synthesized: one surface for search, why, recent, review, import/export, and policy blocks.
  Novel bet: making memoryOSS a visible daily utility instead of an invisible backend.
- `P12-T4 ambient connector mesh`
  Borrowed: sidecars, connectors, and context-ingestion systems.
  Synthesized: many opt-in sources feeding one candidate/evidence/review model.
  Novel bet: a unified memory sensor mesh rather than app-specific integrations.
- `P12-T5 memory primitive algebra`
  Borrowed: loosely inspired by compositional representation learning, world models, and primitive induction patterns.
  Synthesized: policy/constraint/incident/habit/environment/evidence/task-state primitives plus transfer operators for memory.
  Novel bet: this is one of the strongest original wagers in the roadmap.
- `P12-T6 distilled memory coprocessor`
  Borrowed: Toolformer and the general small-specialist-model pattern.
  Synthesized: a local specialist for gate/classify/merge/redact/policy-check/compile decisions.
  Novel bet: memoryOSS-specific coprocessing as a privacy and cost moat.
- `P12-T7 team memory governance`
  Borrowed: code-review and branch-governance patterns.
  Synthesized: memory branches, owners, approvals, watchlists, scoped review and replay-aware governance.
  Novel bet: treating team memory with code-like governance maturity.
- `P12-T8 everyday utility loop`
  Borrowed: product proof and utility benchmark thinking.
  Synthesized: repeated-context elimination, portability, replay fidelity, blocked bad actions, and review throughput in one public suite.
  Novel bet: proving not only that memoryOSS works, but that removing it hurts daily work.
- `P12-T9 universal memory reader`
  Borrowed: universal reader product patterns from portable file utilities.
  Synthesized: safe preview, provenance, diff, and signature inspection for AI-memory bundles.
  Novel bet: making AI-memory artifacts independently readable before import.
- `P12-T10 trust and revocation fabric`
  Borrowed: package-signing and trust-chain systems.
  Synthesized: trust semantics for bundles, passports, authors, devices, and sync peers.
  Novel bet: portable memory with first-class revocation and compromise recovery.
- `P12-T11 zero-friction update plane`
  Borrowed: modern desktop/runtime update systems.
  Synthesized: signed multi-platform install, update, rollback, and schema migration for a local-first memory runtime.
  Novel bet: none; this is required utility-grade productization.
- `P12-T12 compatibility and LTS`
  Borrowed: long-lived format/runtime support discipline.
  Synthesized: compatibility matrix, deprecation policy, and automated old-version fixture checks for memory artifacts.
  Novel bet: none; this is the trust tax every true utility must pay.
