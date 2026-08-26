# Alice Household Chat MVP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (- [ ]) syntax for tracking.

**Goal:** Добавить в alice-llm-bridge изолированный Codex household mode: один persistent thread на дом, несколько разрешённых аккаунтов/колонок, owner-approved pairing, безопасный Homey contract и deterministic voice continuation.

**Architecture:** Legacy OpenAI flow сохраняется. Новый HouseholdEngine маршрутизирует Yandex account/application identity через HouseholdStore, хранит только encrypted temporary state и вызывает отдельный codex-runtime JSON-RPC client по Unix socket; долговременная история остаётся в Codex thread. Webhook выбирает legacy или household backend конфигурацией.

**Tech Stack:** Rust 2024 workspace, Tokio, Axum, Serde/serde_json, SQLx/Postgres, ChaCha20-Poly1305, HMAC-SHA256, Codex app-server JSON-RPC over Unix socket.

**Spec:** docs/superpowers/specs/2026-08-27-alice-household-chat-design.md

## Global Constraints

- Один дом имеет ровно один persistent Codex thread; Postgres не хранит долговременный transcript.
- Один application_id принадлежит одному дому; несколько аккаунтов и поверхностей могут принадлежать одному дому.
- Yandex text и tts не превышают 1024 символа; рабочий chunk limit — 850 вместе с вопросом «Продолжать?».
- Reply budget — 2800 ms; deferred TTL — 120 s; continuation TTL — 600 s; pairing TTL — 600 s.
- Codex app-server доступен только по Unix socket, с read-only sandbox и отдельным household permission profile.
- Любой shell/file/unknown-tool approval отклоняется.
- Homey mutation считается успешной только после verified=true; locks, gates, security, ovens and heaters запрещены в MVP.
- Raw Yandex IDs, utterances, tokens, pairing codes and encrypted payloads не логируются.
- Legacy OpenAI/family-profile path не удаляется и проходит прежние тесты.
- Production behavior появляется только после наблюдаемого RED и завершается GREEN.

---

### Task 1: Portable Rust test runner and clean baseline

**Files:**
- No tracked files.
- Use ignored paths: .dev/rustup, .dev/cargo, .dev/rustup-init.

**Interfaces:**
- Produces executable .dev/cargo/bin/cargo for every later verification command.
- Every later cargo command is executed with CARGO_HOME="$PWD/.dev/cargo", RUSTUP_HOME="$PWD/.dev/rustup" and PATH="$PWD/.dev/cargo/bin:$PATH".

- [ ] **Step 1: Download an isolated official rustup installer**

~~~bash
arch=$(uname -m)
case "$arch" in
  arm64) target=aarch64-apple-darwin ;;
  x86_64) target=x86_64-apple-darwin ;;
  *) echo "unsupported macOS arch: $arch" >&2; exit 1 ;;
esac
curl --proto '=https' --tlsv1.2 --fail --location \
  "https://static.rust-lang.org/rustup/dist/$target/rustup-init" \
  --output .dev/rustup-init
chmod 700 .dev/rustup-init
~~~

- [ ] **Step 2: Install stable without changing user PATH**

~~~bash
CARGO_HOME="$PWD/.dev/cargo" RUSTUP_HOME="$PWD/.dev/rustup" \
  .dev/rustup-init -y --no-modify-path --profile minimal --default-toolchain stable
~~~

- [ ] **Step 3: Verify baseline**

Run: CARGO_HOME="$PWD/.dev/cargo" RUSTUP_HOME="$PWD/.dev/rustup" .dev/cargo/bin/cargo test --workspace

Expected: all imported tests PASS. A failure stops feature work for baseline diagnosis.

---

### Task 2: Deterministic reply shaping and continuation

**Files:**
- Create: crates/bridge-core/src/reply.rs
- Modify: crates/bridge-core/src/lib.rs
- Test: unit tests in reply.rs

**Interfaces:**

~~~rust
pub struct ShapedReply { pub spoken: String, pub remaining: Vec<String> }
pub enum ContinuationDecision { Continue, Stop, Empty }
pub struct ReplyShaper { limit: usize }
impl ReplyShaper {
    pub fn new(limit: usize) -> Self;
    pub fn split(&self, text: &str) -> ShapedReply;
}
impl ContinuationDecision {
    pub fn from_utterance(text: &str) -> Self;
}
~~~

- [ ] **Step 1: Write failing tests**

~~~rust
#[test]
fn long_reply_preserves_text_and_stays_under_limit() {
    let input = format!("{} {}", "А".repeat(700), "Б".repeat(300));
    let shaped = ReplyShaper::new(850).split(&input);
    assert!(shaped.spoken.ends_with(" Продолжать?"));
    assert!(shaped.spoken.chars().count() <= 850);
    let rebuilt = format!(
        "{}{}",
        shaped.spoken.trim_end_matches(" Продолжать?"),
        shaped.remaining.join("")
    );
    assert_eq!(rebuilt, input);
}

#[test]
fn explicit_refusal_stops_but_any_other_word_continues() {
    for value in ["нет", "Не надо!", "не продолжай", "хватит", "стоп", "отмена"] {
        assert_eq!(ContinuationDecision::from_utterance(value), ContinuationDecision::Stop);
    }
    assert_eq!(ContinuationDecision::from_utterance("ага"), ContinuationDecision::Continue);
    assert_eq!(ContinuationDecision::from_utterance("включи свет"), ContinuationDecision::Continue);
    assert_eq!(ContinuationDecision::from_utterance("   "), ContinuationDecision::Empty);
}
~~~

- [ ] **Step 2: Run RED**

Run: cargo test -p bridge-core reply::tests

Expected: FAIL because the reply module is absent.

- [ ] **Step 3: Implement minimal splitter**

Use suffix " Продолжать?" and normalized stop set ["нет", "не надо", "не продолжай", "хватит", "стоп", "отмена"]. Prefer paragraph/sentence boundary, then whitespace, then exact Unicode char boundary. Every source character belongs to exactly one chunk.

- [ ] **Step 4: Run GREEN**

Run: cargo test -p bridge-core reply::tests

Expected: PASS; changing 850 to 851 or removing a refusal makes a test fail.

- [ ] **Step 5: Commit**

~~~bash
git add crates/bridge-core/src/reply.rs crates/bridge-core/src/lib.rs
git commit -m "feat: add deterministic Alice reply continuation"
~~~

---

### Task 3: Household domain and runtime ports

**Files:**
- Create: crates/bridge-core/src/household.rs
- Create: crates/bridge-core/src/house_store.rs
- Create: crates/bridge-core/src/house_runtime.rs
- Modify: crates/bridge-core/src/lib.rs
- Modify: crates/bridge-core/src/testing.rs
- Test: unit tests in new modules

**Interfaces:**

~~~rust
pub struct SurfaceIdentity { pub user_id: String, pub application_id: String }
impl SurfaceIdentity { pub fn new(user_id: impl Into<String>, application_id: impl Into<String>) -> Self; }
pub struct HouseContext {
    pub id: i64,
    pub name: String,
    pub codex_thread_id: Option<String>,
    pub homey_connector_id: String,
}
pub enum SurfaceResolution {
    Bound(HouseContext),
    PairingRequired { spoken_code: String },
    Unauthorized,
}
pub enum PendingReply { None, Thinking, Ready(String) }
pub type StoreResult<T> = Result<T, HouseholdStoreError>;
pub type RuntimeResult<T> = Result<T, RuntimeError>;

#[async_trait::async_trait]
pub trait HouseholdStore: Send + Sync {
    async fn resolve_surface(&self, identity: &SurfaceIdentity) -> StoreResult<SurfaceResolution>;
    async fn save_thread_id(&self, house_id: i64, thread_id: &str) -> StoreResult<()>;
    async fn poll_pending(&self, house_id: i64, application_id: &str) -> StoreResult<PendingReply>;
    async fn mark_thinking(&self, house_id: i64, application_id: &str) -> StoreResult<()>;
    async fn save_ready(&self, house_id: i64, application_id: &str, text: &str) -> StoreResult<()>;
    async fn clear_pending(&self, house_id: i64, application_id: &str) -> StoreResult<()>;
    async fn take_continuation(&self, house_id: i64, application_id: &str) -> StoreResult<Option<Vec<String>>>;
    async fn save_continuation(&self, house_id: i64, application_id: &str, chunks: &[String]) -> StoreResult<()>;
    async fn clear_continuation(&self, house_id: i64, application_id: &str) -> StoreResult<()>;
}

#[async_trait::async_trait]
pub trait HouseRuntime: Send + Sync {
    async fn start_thread(&self, house: &HouseContext, instructions: &str) -> RuntimeResult<String>;
    async fn turn(&self, thread_id: &str, utterance: &str) -> RuntimeResult<String>;
}
~~~

- [ ] **Step 1: Write failing routing tests**

~~~rust
#[tokio::test]
async fn two_accounts_and_surfaces_resolve_to_one_thread() {
    let store = MemoryHouseholdStore::fixture()
        .house(1, "Дом мамы", Some("thread-1"))
        .member(1, "OWNER")
        .member(1, "MOTHER")
        .surface(1, "OWNER", "owner-phone")
        .surface(1, "MOTHER", "mother-station");
    for identity in [
        SurfaceIdentity::new("OWNER", "owner-phone"),
        SurfaceIdentity::new("MOTHER", "mother-station"),
    ] {
        let SurfaceResolution::Bound(house) =
            store.resolve_surface(&identity).await.unwrap() else { panic!() };
        assert_eq!(house.id, 1);
        assert_eq!(house.codex_thread_id.as_deref(), Some("thread-1"));
    }
}
~~~

Add a second test proving an enabled member with an unknown application gets PairingRequired while a stranger gets Unauthorized without a house name.

- [ ] **Step 2: Run RED**

Run: cargo test -p bridge-core household house_store house_runtime

Expected: FAIL with missing modules/types.

- [ ] **Step 3: Implement types, traits and MemoryHouseholdStore**

The fake enforces application_id → one house and never returns a house name to an unauthorized identity.

- [ ] **Step 4: Run GREEN and commit**

~~~bash
cargo test -p bridge-core household house_store house_runtime
git add crates/bridge-core/src
git commit -m "feat: define household routing ports"
~~~

---

### Task 4: Encrypted Postgres household state

**Files:**
- Create: migrations/0002_household.sql
- Create: crates/bridge-server/src/house_store_pg.rs
- Create: crates/bridge-server/src/state_crypto.rs
- Modify: Cargo.toml
- Modify: crates/bridge-server/Cargo.toml
- Modify: crates/bridge-server/src/lib.rs
- Test: unit tests in state_crypto.rs and SQLx tests in house_store_pg.rs

**Interfaces:**

~~~rust
pub struct EncryptedPayload { pub nonce: Vec<u8>, pub ciphertext: Vec<u8>, pub key_version: i16 }
pub struct StateCipher { key: [u8; 32], key_version: i16 }
impl StateCipher {
    pub fn from_hex(value: &str) -> Result<Self, CryptoError>;
    pub fn seal(&self, plaintext: &[u8]) -> Result<EncryptedPayload, CryptoError>;
    pub fn open(&self, payload: &EncryptedPayload) -> Result<Vec<u8>, CryptoError>;
}
pub struct PgHouseholdStore { pool: PgPool, cipher: StateCipher }
~~~

- [ ] **Step 1: Write failing crypto tests**

Roundtrip must return exact UTF-8 bytes. Flipping one ciphertext byte must fail authentication. Two seals of the same plaintext must produce different nonces and ciphertext.

- [ ] **Step 2: Write failing SQLx tests**

Prove cross-house isolation, exact user/application binding, 600-second pairing expiry, 120-second pending expiry, 600-second continuation expiry, ciphertext absence of plaintext, and refusal to overwrite a non-null different codex_thread_id.

- [ ] **Step 3: Run RED**

Run: cargo test -p bridge-server state_crypto house_store_pg

Expected: FAIL because modules, migration and dependencies are absent.

- [ ] **Step 4: Add minimum dependencies**

~~~toml
chacha20poly1305 = "0.10"
hmac = "0.12"
sha2 = "0.10"
hex = "0.4"
rand = "0.9"
~~~

Migration creates houses, house_members, surfaces, pairing_requests, pending_replies and continuations with foreign keys, unique surfaces.application_id, unique (house_id, yandex_user_id), BYTEA payload/nonce fields and TTL indexes. Pairing code hashes use HMAC-SHA256 keyed by STATE_ENCRYPTION_KEY.

- [ ] **Step 5: Implement StateCipher and PgHouseholdStore**

STATE_ENCRYPTION_KEY is exactly 64 hex characters. Every ChaCha20-Poly1305 write uses a fresh 96-bit nonce. Resolution first checks enabled membership, then exact user/application binding.

- [ ] **Step 6: Run GREEN and commit**

~~~bash
cargo test -p bridge-server state_crypto house_store_pg
git add Cargo.toml Cargo.lock migrations crates/bridge-server
git commit -m "feat: persist encrypted household routing state"
~~~

---

### Task 5: Owner-approved pairing CLI

**Files:**
- Create: crates/bridge-server/src/bin/bridge-admin.rs
- Modify: crates/bridge-server/src/house_store_pg.rs
- Test: crates/bridge-server/tests/admin_cli.rs

**Interfaces:**

~~~text
bridge-admin house create --name NAME --homey-connector ID
bridge-admin member add --house ID --user-id ID --role owner|member
bridge-admin pairing approve --house ID --code SIX_DIGITS
bridge-admin member disable --house ID --user-id ID
bridge-admin surface disable --application-id ID
~~~

- [ ] **Step 1: Write failing end-to-end CLI test**

With disposable DATABASE_URL and STATE_ENCRYPTION_KEY: create house/member, insert pairing request through PgHouseholdStore, approve it through the binary, assert the exact surface resolves and another application does not.

- [ ] **Step 2: Run RED**

Run: cargo test -p bridge-server --test admin_cli

Expected: FAIL because bridge-admin does not exist.

- [ ] **Step 3: Implement fixed std::env argument parser**

Do not add a CLI framework. Reject missing/extra flags, require exact six digits, and never print raw Yandex IDs after success.

- [ ] **Step 4: Run GREEN and commit**

~~~bash
cargo test -p bridge-server --test admin_cli
git add crates/bridge-server/src/bin/bridge-admin.rs crates/bridge-server/src/house_store_pg.rs crates/bridge-server/tests/admin_cli.rs
git commit -m "feat: add household pairing admin CLI"
~~~

---

### Task 6: Codex app-server JSON-RPC client

**Files:**
- Create: crates/codex-runtime/Cargo.toml
- Create: crates/codex-runtime/src/lib.rs
- Create: crates/codex-runtime/src/client.rs
- Create: crates/codex-runtime/src/protocol.rs
- Create: crates/codex-runtime/src/error.rs
- Create: crates/codex-runtime/tests/jsonrpc.rs
- Modify: root Cargo.toml

**Interfaces:**

~~~rust
pub struct CodexRuntimeConfig {
    pub socket_path: PathBuf,
    pub cwd_root: PathBuf,
    pub permission_profile_prefix: String,
}
pub struct CodexRuntime { config: CodexRuntimeConfig }
~~~

CodexRuntime implements HouseRuntime.

- [ ] **Step 1: Write failing duplex-transport test**

Use tokio::io::duplex. Assert request order initialize → initialized → thread/start. Thread start contains absolute house cwd, permissions "alice-house-1", sandbox "read-only" and exact developerInstructions. Return thread-1, then assert turn/start receives one text UserInput and agentMessage/delta is concatenated until turn/completed.

- [ ] **Step 2: Write failing denial/error tests**

commandExecution/requestApproval, fileChange/requestApproval and unknown server requests must receive denial or return RuntimeError; JSON-RPC errors, mismatched thread IDs and EOF never fabricate assistant text.

- [ ] **Step 3: Run RED**

Run: cargo test -p codex-runtime

Expected: FAIL because crate/client do not exist.

- [ ] **Step 4: Implement minimal JSONL codec**

Support only initialize, initialized, thread/start, thread/resume, turn/start, agentMessage/delta, turn/completed and approval denial. Do not expose command/process/fs methods.

- [ ] **Step 5: Run GREEN and schema check**

~~~bash
cargo test -p codex-runtime
schema_dir=$(mktemp -d)
codex app-server generate-json-schema --experimental --out "$schema_dir"
rg 'thread/start|thread/resume|turn/start|agentMessage/delta|turn/completed' "$schema_dir"
~~~

Expected: tests PASS and methods appear in local Codex 0.149 schema. Spain schema remains a read-only canary gate.

- [ ] **Step 6: Commit**

~~~bash
git add Cargo.toml Cargo.lock crates/codex-runtime
git commit -m "feat: add Codex app-server runtime client"
~~~

---

### Task 7: HouseholdEngine orchestration and prompt

**Files:**
- Create: crates/bridge-core/src/house_engine.rs
- Create: crates/bridge-core/src/house_prompt.rs
- Modify: crates/bridge-core/src/lib.rs
- Modify: crates/bridge-core/src/testing.rs
- Test: unit tests in house_engine.rs and house_prompt.rs

**Interfaces:**

~~~rust
pub struct HouseholdEngineConfig { pub reply_budget: Duration, pub chunk_limit: usize }
pub struct HouseholdInput {
    pub identity: SurfaceIdentity,
    pub utterance: String,
    pub new_session: bool,
}
pub enum HouseholdReply { Say(String), Refuse, Pairing(String), Busy, InternalError }
pub struct HouseholdEngine {
    store: Arc<dyn HouseholdStore>,
    runtime: Arc<dyn HouseRuntime>,
    shaper: ReplyShaper,
    locks: DashMap<i64, Arc<tokio::sync::Mutex<()>>>,
    config: HouseholdEngineConfig,
}
pub fn build_house_instructions(house: &HouseContext) -> String;
~~~

- [ ] **Step 1: Write failing orchestration tests**

Prove two account/surface inputs resolved to one house call one thread; continuation consumes "включи свет" without a new runtime turn; stop clears and says "Хорошо."; 2800 ms timeout marks pending; only initiating application receives ready text; concurrent second surface gets Busy.

- [ ] **Step 2: Run RED**

Run: cargo test -p bridge-core house_engine house_prompt

Expected: FAIL because engine/prompt are absent.

- [ ] **Step 3: Implement fixed state precedence**

~~~text
resolve surface
→ continuation
→ pending
→ empty new-session greeting
→ per-house lock
→ start missing thread once or resume existing
→ runtime turn with 2800 ms timeout
→ shape and persist temporary continuation
~~~

Use DashMap<house_id, Arc<tokio::sync::Mutex<()>>>. Code-local ceiling: correct for one bridge replica. Measurable upgrade trigger is replica_count > 1; upgrade path is a Postgres advisory lock keyed by house ID.

- [ ] **Step 4: Implement exact spec prompt**

Interpolate only house.name. Tests assert Russian response, 850-char default, current-house tools only, verified read-back, maximum one attention item and prohibition of high-risk devices.

- [ ] **Step 5: Run GREEN and commit**

~~~bash
cargo test -p bridge-core house_engine house_prompt
git add crates/bridge-core/src
git commit -m "feat: orchestrate persistent household conversations"
~~~

---

### Task 8: Config, assembly and webhook integration

**Files:**
- Modify: crates/bridge-server/src/config.rs
- Modify: crates/bridge-server/src/assemble.rs
- Modify: crates/bridge-server/src/routes.rs
- Modify: crates/bridge-server/src/main.rs
- Modify: crates/bridge-server/tests/webhook.rs
- Modify: crates/bridge-server/Cargo.toml
- Modify: config.example.toml

**Interfaces:**

~~~toml
[runtime]
mode = "household_codex"
codex_socket = "/run/alice-codex/app-server.sock"
codex_cwd_root = "/srv/alice/houses"
permission_profile_prefix = "alice-house-"
chunk_limit = 850
~~~

~~~rust
pub enum SkillBackend { Legacy(Engine), Household(HouseholdEngine) }
~~~

- [ ] **Step 1: Write failing config tests**

Old config parses as legacy. Household mode requires absolute socket/cwd paths and chunk_limit <= 900, and does not resolve OpenAI API keys during household assembly.

- [ ] **Step 2: Write failing webhook tests**

Add application_id to fixtures. Prove wrong secret 404; stranger closes; unbound member receives pairing without house name; two account/surface requests hit one fake thread; two houses hit different threads; every text/tts is <= 1024.

- [ ] **Step 3: Run RED**

Run: cargo test -p bridge-server config webhook

Expected: FAIL because household wiring does not exist.

- [ ] **Step 4: Implement backward-compatible backend selection**

Household main constructs StateCipher, PgHouseholdStore, CodexRuntime and HouseholdEngine. Legacy main keeps build_engine. Logs contain only correlation ID and sanitized outcome.

- [ ] **Step 5: Run GREEN and legacy regression**

~~~bash
cargo test -p bridge-server
cargo test --workspace
~~~

Expected: new and legacy tests PASS.

- [ ] **Step 6: Commit**

~~~bash
git add crates/bridge-server config.example.toml Cargo.lock
git commit -m "feat: wire household Codex mode into Alice webhook"
~~~

---

### Task 9: Homey contract, operator docs and acceptance suite

**Files:**
- Create: crates/codex-runtime/tests/homey_contract.rs
- Create: docs/household-setup.md
- Modify: README.md
- Modify: docs/skill-setup.md
- Modify: config.example.toml
- Modify: this plan to check completed steps

**Interfaces:**
- House-scoped MCP surface is exactly list_attention_items(), get_device_state(device_id), set_device_capability(device_id, capability, value).

- [ ] **Step 1: Write failing fake Homey flow tests**

Fixture one returns requested=true, observed=true, verified=true plus two warnings; final speech must confirm observed state and contain exactly one highest-priority warning. Fixture two returns verified=false; final speech must not say "включён".

- [ ] **Step 2: Run RED**

Run: cargo test -p codex-runtime --test homey_contract

Expected: FAIL until protocol/prompt handling is complete.

- [ ] **Step 3: Complete minimal tool lifecycle handling**

Bridge observes normal MCP lifecycle but never handles Homey OAuth/tokens. Approval requests remain denied. The external gateway owns allowlist and read-back.

- [ ] **Step 4: Document exact setup**

docs/household-setup.md covers: create house, add owner/member, share private Yandex link, bind each surface, start isolated app-server, register only house gateway, run read-only canary, revoke access, rollback without deleting thread.

- [ ] **Step 5: Run final verification**

~~~bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
git diff --check
git status --short
~~~

Expected: all checks PASS and worktree is clean after commit.

- [ ] **Step 6: Commit**

~~~bash
git add README.md docs config.example.toml crates/codex-runtime/tests/homey_contract.rs
git commit -m "docs: add household setup and Homey safety gates"
~~~

## Final live gates

Local completion does not authorize live mutation:

1. Push feature branch and confirm CI format/clippy/test PASS.
2. Import Spain read-only canary from visible Alice ChatGPT task.
3. Verify exact Spain app-server schema and Unix transport.
4. Deploy bridge plus isolated app-server without Homey writes.
5. Run Yandex fixture and one real-column conversation canary.
6. Enumerate Homey through read-only gateway.
7. Stop and request owner approval naming the exact reversible device/action before the first live write.
