# Kotoha 設計仕様書 (DESIGN.md)

**Single Source of Truth** — 本ドキュメントは、Kotoha の C++ プラグイン、Rust コア、Tauri v2 / React UI、および GitHub Actions CI/CD 実装が従う唯一の確定仕様である。

---

## 1. 概要と設計哲学 (Overview & Design Philosophy)

**Kotoha** は、ボイスチャット（Mumble を第一ターゲットとし、将来的に TeamSpeak 3 や OS ループバックへ拡張）の音声をリアルタイムに文字起こし・翻訳し、ゲームや作業画面の上に透過クリックスルー字幕として表示するマルチプラットフォーム（Windows / macOS / Linux）デスクトップアプリケーションである。

### 設計原則

1. **Zero-Config Default（ゼロ設定デフォルト）**
   - 初回起動時のセットアップウィザードにより、ビルド済みプラグインの自動配置、ローカル STT / ローカル NMT モデルのダウンロード・SHA-256 検証・展開を自動化する。API キーや外部クラウド契約なしに即座に動作開始できる。
2. **Subtitle-Only（字幕特化・TTS 非搭載）**
   - 音声合成（TTS）や仮想マイク出力は持たず、純粋に「聴く・読む」字幕体験（低遅延・非干渉・クリックスルーオーバーレイ）に集中する。
3. **Thin Plugin / Thick App（薄いプラグイン / 厚いホストアプリ）**
   - ボイスチャット側プラグイン（C++20）は「音声バッファ・話者状態の取得と OS ネイティブ IPC 送信」のみに責務を限定する。DSP リサンプリング、VAD、STT、翻訳、会話履歴管理、UI 描画はすべて独立プロセスである Rust コア／Tauri アプリ側で実行する。これによりホスト側（Mumble 本体等）のクラッシュリスクと音声スレッド遅延を最小化する。
4. **Multi-Source Extensibility（複数入力ソース拡張性）**
   - IPC プロトコルおよび Rust コアの話者モデル（`SpeakerKey { source, user_id }`）により、Mumble プラグインを皮切りに TeamSpeak 3 プラグインや OS ループバック入力（WASAPI / CoreAudio / PipeWire）を同一パイプラインで透過的に扱える。
5. **Pluggable STT & Translation & Clean/Hexagonal Architecture（差し替え可能な STT・翻訳とクリーンアーキテクチャ）**
   - Rust コアを **Clean / Hexagonal (Onion) Architecture**（`domain` ← `application` ← `infrastructure` / `presentation`）で構成し、Port Trait（`SttEngine`, `Translator`, `AudioInputSource`, `SubtitleSink`, `SettingsRepository`）を通じて外部依存を疎結合化する。
   - 翻訳プロバイダは実行時ホットスワップ可能とし、ハードウェア環境（CPU / GPU CUDA / Apple Silicon Metal）に応じたモデル単位の選択（軽量 NMT / 多言語 NMT / ローカル LLM）、およびクラウド API（OpenAI 互換 / DeepL / Google Cloud Translation / ユーザー自作 GAS Webhook）をサポートする。
6. **Multi-Language（多言語対応）**
   - 原語（`src_lang`）および訳語（`tgt_lang`）は `LanguageCode`（ISO 639-1 または BCP-47、例: `"en"`, `"ja"`, `"zh"`, `"ko"`, `"en-US"`, `"pt-BR"`）で管理し、初期デフォルトは `src_lang = "en"`, `tgt_lang = "ja"` としつつ多言語拡張可能な設計とする。
7. **Multi-Platform（3OS 対応）**
   - Windows 10/11 (x86_64)、macOS (arm64 / x86_64 Universal または各アーキテクチャ)、Linux (x86_64: X11 / XWayland / Wayland) を単一コードベース・3OS CI マトリクスでサポートする。

---

## 2. システム全体アーキテクチャ (System Architecture)

```text
+===================================================================================+
| Voice Chat Host Process (例: Mumble Client, 将来: TeamSpeak 3)                    |
|                                                                                   |
|  [Audio Callback Thread (Real-Time)]        [Control / Callback Thread]           |
|   mumble_onAudioSourceFetched(...)           mumble_onUserTalkingStateChanged     |
|    - isSpeech == true のみ抽出               mumble_onUserAdded / Removed         |
|    - Heap alloc / Lock / I/O 厳禁            mumble_onServerSynchronized/Disc.    |
|    - 常に false 返却 (音声改変なし)            |                                  |
|          |                                     v                                  |
|          v (try_push: 満杯時はDrop-New, seq消費)   [Mutex-protected Control Queue]     |
|   [Lock-Free SPSC Ring Buffer]                 |                                  |
|          |                                     |                                  |
+----------|-------------------------------------|----------------------------------+
           |                                     |
           +-----------------+-------------------+
                             v
               [IPC Worker Thread (C++20)]
                - 1s 自動再接続ループ
                - 1s Heartbeat (0x04) 送信
                - 2-5ms bounded wait / drain順序制御
                - Little-Endian Binary Framing (Kotoha IPC v1)
                             |
     ========================|======================== OS Native IPC
       Windows: \\.\pipe\kotoha_v1 (Named Pipe)
       Linux/macOS: $XDG_RUNTIME_DIR/kotoha_v1.sock (UDS, mode 0600)
                    or ${TMPDIR:-/tmp}/kotoha-<euid>/kotoha_v1.sock (dir 0700)
       Override: KOTOHA_IPC_ENDPOINT
     ========================|========================
                             v
+===================================================================================+
| Kotoha Desktop Application Process (Tauri v2 + Rust Core: Clean / Hexagonal)      |
|  [Dependency Rule: Outer -> Inner Only (Presentation/Infra -> Application -> Domain)]
|                                                                                   |
|  [Infrastructure Layer (Adapters)]                                                |
|   - IpcServerAdapter (tokio + interprocess + kotoha-protocol) -> AudioInputSource |
|       * 並列 Accept / conn_id 所有権管理 / Hello 厳格検証 / seq 回帰ガード        |
|   - AudioPreprocessor (任意 ch -> モノラル平均, 8k..192kHz -> 16kHz f32)          |
|   - STT Adapters: MockSttAdapter (P1) / kotoha-stt-sherpa (P2)                    |
|   - Translator Adapters: MockTranslator (P1) / kotoha-translate-local (ct2rs)     |
|                          / kotoha-translate-http (OpenAI互換, DeepL, Google, GAS) |
|          |                                     ^                                  |
|          v (implements / drives)               | (implements Ports)                 |
|  [Application Layer (Use Cases & Orchestration)]                                  |
|   - SourceSessionManager (conn_id 所有権チェック, SourceReset/EOF 解放)           |
|   - PipelineService (音声前処理 -> 話者別 SttStream[Hotwords] -> Pre-filter ->    |
|                      Context/Glossary -> Semaphore並列翻訳 -> SubtitleSink)       |
|   - TranslatorManager (実行時プロバイダ & ModelProfile ホットスワップ)            |
|   - GlossaryManager (glossaries/*.toml 読込・優先マージ -> STT & Translator 共有) |
|          |                                                                        |
|          v (depends ONLY on)                                                      |
|  [Domain Layer (Innermost Core: Zero I/O)]                                        |
|   - Entities/VOs: SpeakerKey, LanguageCode, AudioChunk, UtteranceId,              |
|     TranscriptEvent, TranslationRequest/Result, TranslateContext, SubtitleEntry,  |
|     GlossaryProfile, ModelProfile, ComputeDevice, TranslationProviderConfig       |
|   - Ports (Traits): SttEngine, SttStream, Translator, AudioInputSource,           |
|                     SubtitleSink, SettingsRepository, GlossaryRepository          |
|   - Pure Logic: Pre-filter, 話者カラー, Context Window, 用語プレースホルダ保護/復元 |
|          ^                                                                        |
|          | (implements SubtitleSink / calls Use Cases)                            |
|  [Presentation Layer (Outermost)]                                                 |
|   - Phase 1: kotoha-ipc-monitor CLI                                               |
|   - Phase 3: Tauri v2 (Tray / Settings / Wizard / Event Bridge)                   |
|              + Overlay Window (React + TS + Vite, 透過クリックスルー 3段階更新)   |
+===================================================================================+
```

---

## 3. IPC プロトコル v1 バイナリ仕様 (IPC Protocol v1 Specification)

本セクションは C++ (`plugins/common`) と Rust (`crates/kotoha-protocol`) の間で **1 バイトの曖昧さもなく** 相互運用するための正規仕様である。

### 3.1 エンドポイント解決規則

C++ クライアント（`resolve_ipc_endpoint` / `resolve_posix_ipc_endpoint`）・Rust サーバー（`kotoha_protocol::resolve_endpoint_details`）双方が同一の優先順位でエンドポイントを決定する。両実装の一致は共有フィクスチャ `tests/fixtures/endpoint_resolution_cases.tsv` を Rust・C++ 双方のテストで検証することで担保する。

| 優先度 | 条件 / OS | エンドポイント形式 | 備考 |
| :--- | :--- | :--- | :--- |
| 1 | 環境変数 `KOTOHA_IPC_ENDPOINT` が非空文字列として設定されている | `KOTOHA_IPC_ENDPOINT` の値そのまま | 単体・E2E テストでの一時エンドポイント分離に使用 |
| 2 (Windows) | `KOTOHA_IPC_ENDPOINT` 未設定または空 | `\\.\pipe\kotoha_v1` | Windows Named Pipe（Byte-stream モード） |
| 2 (Linux / macOS) | `KOTOHA_IPC_ENDPOINT` 未設定または空かつ `$XDG_RUNTIME_DIR` が非空 | `$XDG_RUNTIME_DIR/kotoha_v1.sock` | 末尾のスラッシュは正規化して連結 |
| 3 (Linux / macOS) | `$XDG_RUNTIME_DIR` 未設定または空 | `${TMPDIR}/kotoha-<euid>/kotoha_v1.sock` | ユーザー専用ディレクトリ（下記）。`$TMPDIR` が未設定・空・相対パス・`/` のみ、または結果が **90 バイト超**の場合は `/tmp/kotoha-<euid>/kotoha_v1.sock` を使う。`<euid>` は実効 UID の 10 進表記。`$TMPDIR` 末尾のスラッシュは正規化する |

- `/tmp/kotoha_v1.sock` のような **誰でも書き込める共有ディレクトリ直下へのフォールバックは行わない**（別ユーザーが先に同じパスで listen すると、プラグインが全参加者の音声と表示名をそのサーバーへ送ってしまうため）。macOS の GUI 起動アプリには通常 `XDG_RUNTIME_DIR` が無いが、`$TMPDIR`（`/var/folders/.../T/`）はユーザーごとのディレクトリなので優先度 3 の第一候補になる。
- **ユーザー専用ディレクトリ（優先度 3）の扱い**: サーバーは bind 前に当該ディレクトリが無ければ **mode `0700`** で作成する（親ディレクトリは作成しない）。既存の場合を含め、`lstat` 結果が (a) 実ディレクトリである（シンボリックリンク不可）、(b) 所有者が実効 UID、(c) group/other のパーミッションビットが無い（`mode & 0o077 == 0`）、のすべてを満たさなければ**使用を拒否してエラー終了**する（パーミッションの自動修正もしない）。`KOTOHA_IPC_ENDPOINT` と `$XDG_RUNTIME_DIR` の場合はこの検査を行わない（`$XDG_RUNTIME_DIR` は仕様上 `0700` かつ本人所有）。

#### Unix Domain Socket (Linux / macOS) 固有要件
- **パーミッション**: サーバーは `bind` 完了直後（または `umask` 制御により作成時）にソケットファイルのパーミッションを **`0600`**（所有者のみ読み書き可）に設定し、他ユーザーからの盗聴・注入を防止する。
- **古いソケットファイル処理**: サーバー起動時、対象パスに何かが存在する場合は `lstat` で種別と所有者を確認する。**ソケットであり、かつ所有者が実効 UID である場合に限り**、接続を試行して `ECONNREFUSED` 等で生存サーバー不在を確認したうえで `unlink` してから `bind` する（生存サーバーがあればエラー終了）。通常ファイル・ディレクトリ・シンボリックリンク（リンク先を問わない）・他ユーザー所有のソケットの場合は**一切削除せず**、理由を示す `CoreError::Ipc` でエラー終了する。
- **終了時のソケット削除**: サーバー正常終了時は、パスが bind 時に作成したソケットそのもの（同一 device/inode・ソケット種別・実効 UID 所有）である場合に限り削除する。実行中に別のファイルへ置き換えられていた場合は削除しない（`interprocess` の name reclamation は無効化し、削除はこの検査付き処理のみで行う）。
- **クライアントによるサーバー検証（なりすまし防止）**: ソケットの `0600` は「他ユーザーがサーバーへ接続すること」は防ぐが「他ユーザーがサーバーになりすますこと」は防げない。そのため C++ クライアントは `connect` 成功直後、`Hello` を含む一切のデータを送る前に、ピアの実効 UID が自プロセスの `geteuid()` と一致することを検証する（Linux: `getsockopt(SO_PEERCRED)`、macOS/BSD: `getpeereid()`、どちらも使えない環境では常に失敗扱い）。不一致・取得失敗時はソケットを閉じて接続失敗として扱い、通常の再接続間隔（§3.2）で再試行する。検証はワーカースレッドで行い、オーディオスレッドの不変条件には影響しない。
- **POSIX `SIGPIPE` 防止 & パス長制限**:
  - C++ クライアントはソケット切断時の `SIGPIPE` による Mumble ホストプロセス強制終了を防ぐため、Linux では `send(..., MSG_NOSIGNAL)`、macOS ではソケット生成直後に `setsockopt(fd, SOL_SOCKET, SO_NOSIGPIPE, ...)` を必須設定する。
  - macOS の `sockaddr_un.sun_path` は最大 **104 バイト**（NUL 終端含む、Linux は 108 バイト）の制約がある。テストや CI で `KOTOHA_IPC_ENDPOINT` に一時パスを指定する際は、常に **90 バイト以下**の短いパス（例: `/tmp/kotoha-test-<pid>.sock`）を使用する。
- **Named Pipe (Windows) 固有要件**:
  - **Rust (`interprocess`) 命名規約**: Rust `interprocess` クレート（v2 `local_socket::Name` / `GenericNamespaced` + `ToNsName`）で名前付きパイプを生成・接続する場合、`ToNsName` はプレフィックス無しのパイプ名（例: `"kotoha_v1"`）から内部的に `\\.\pipe\kotoha_v1` を構築する。一方、`KOTOHA_IPC_ENDPOINT` には Windows 標準形式 `\\.\pipe\<name>` と裸の `<name>` の双方が渡され得るため、Rust 側の Windows エンドポイント解決関数は先頭の `\\.\pipe\`（大文字小文字非区別）を正規化して `ToNsName` に渡す（C++ 側は `\\.\pipe\<name>` をそのまま `CreateFileW` へ渡し、裸名が渡された場合のみ `\\.\pipe\` を補完する）。
  - **アクセス制御・セキュリティ記述子**: パイプは `PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT` 互換のバイトストリームモードで作成し、現行ユーザー SID にアクセスを限定する DACL（Local System / 現行ユーザーのみ許可し、他ユーザー・ネットワークアクセスを拒否するセキュリティ記述子）を適用する。
  - **先行作成（squatting）防止**: サーバーは最初のパイプインスタンスを `FILE_FLAG_FIRST_PIPE_INSTANCE` 付きで作成し、同名パイプが既に存在する場合は起動に失敗する（`interprocess` v2 の `PipeListener` は最初のインスタンス作成時にこのフラグを常に付与する）。
  - **クライアントによるサーバー検証**: C++ クライアントは `CreateFileW` 成功直後、データ送信前に `GetNamedPipeServerProcessId` でサーバープロセス ID を取得し、`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` → `OpenProcessToken(TOKEN_QUERY)` → `GetTokenInformation(TokenUser)` で得たユーザー SID が自プロセスのトークンユーザー SID と `EqualSid` で一致することを確認する。いずれかの API が失敗した場合や不一致の場合はハンドルを閉じて接続失敗として扱う（fail closed）。
  - **C++ 非同期 I/O とシャットダウン非ブロッキング**: C++ クライアントは `CreateFileW` 時に `FILE_FLAG_OVERLAPPED` を指定し、`WriteFile` も `OVERLAPPED` + イベント待機で行うことで、`mumble_shutdown()` 時に `CancelIoEx(handle, nullptr)` により進行中のパイプ I/O を即座にキャンセルしてブロックせずに終了できるようにする。また、接続時に `ERROR_PIPE_BUSY` が返った場合は短い待機（例: `WaitNamedPipeW(endpoint, 50)`）の後に再試行する。

### 3.2 トポロジと接続ライフサイクル

- **トポロジ**:
  - **Rust Core = Server (Listen / Accept)**: 複数クライアントの同時接続を受け付ける（例: Mumble と TS3 の同時接続、または再接続時の旧ソケット遷移）。
  - **C++ Plugin = Client (Connect)**: ホスト起動・プラグイン有効化時に非同期ワーカースレッドが接続を試行する。
- **接続所有権と同一 `source` 再接続レース防止 (`conn_id`)**:
  - `IpcServer` は Accept したすべての接続に単調増加の一意な **`conn_id: u64`** を割り当てる。
  - `Hello` 受信により接続の `source` 名が確定した際、`active_sources: HashMap<String, u64>` に当該 `source -> conn_id` を登録する。既に同一 `source` の別 `conn_id` がアクティブであった場合は即座に新しい `conn_id` で上書きし、**旧接続のタスクを能動的にキャンセルして切断する**（実装: IPC アダプタが `source -> (conn_id, cancel)` の表を保持し、新しい `Hello` 完了時に旧接続へキャンセルを通知する。旧接続は `SourceDisconnected { reason: Superseded }` を送出するが、所有権チェックにより `SourceSessionManager` では無視される）。
  - `SourceReset` 受信時または EOF / I/O エラー検知時のクリーンアップは、**必ず `active_sources.get(&source) == Some(&conn_id)` を検証した上でのみ**実行する。これにより、再接続直後に旧ソケットの遅延 EOF が新接続の話者状態や STT ストリームを誤破棄するレースを防ぐ。さらに、サーバーは「所有権表の更新・旧接続のキャンセル・`SourceConnected` のチャネル投入」を同一ロック内で行い（チャネル容量を先に確保してから）、ほぼ同時の `Hello` でもチャネル上のイベント順と所有権の順序が一致するようにしている。
  - **注意（同名 `source` の多重起動）**: 同じ `source` 名を使う 2 つのプラグインインスタンスが同時に接続すると、再接続のたびに互いを置き換え合い（flapping）、安定して接続できない。`source` 名はホストアプリごとに一意とすること。
- **自動再接続**: 接続失敗または切断検知時、C++ クライアントは **1000 ms (1 秒) 間隔** でリトライする（シャットダウン要求時は 50 ms 以内など速やかにループ離脱できるよう条件変数 `wait_for` で待機する）。サーバー身元検証（§3.1: ピア UID / パイプサーバー SID）の不一致も接続失敗として扱い、同じ間隔で再試行する。
- **セッション初期化 (`Hello`) 厳密ルール**:
  - クライアントは接続確立直後、他のどのメッセージよりも先に **`Hello` (`0x00`)** を 1 回送信しなければならない。
  - **第 1 フレームが `Hello` 以外**、**`Hello` のデコード検証失敗**、または **セッション途中での重複 `Hello` 受信** はすべて `ProtocolError` とみなし、サーバーは即座に接続を切断する。
- **すべての `ProtocolError` における切断原則**:
  - ヘッダ検証エラー（`InvalidMagic`, `UnsupportedVersion`, `PayloadTooLarge`, `UnknownMessageType`）およびペイロード検証エラー（`InvalidPayloadLength`, `InvalidUtf8`, `InvalidFlags`, `InvalidPcmSample`, `InvalidTalkingState` 等）が発生した場合、フレーミング境界が喪失またはピアが破損しているため、デコーダはエラーを返し、サーバーは**必ず該当ピア接続を即座に切断**する（リカバー継続しない）。
- **Keepalive (`Heartbeat`)**:
  - クライアントは送信する他メッセージがない場合を含め、原則 **1000 ms (1 秒) ごと** に `Heartbeat` (`0x04`) を送信する。
  - サーバーは生存監視と UI の接続ステータス表示（「接続中 / 未接続」）に `Heartbeat` を利用する。実装済みのタイムアウト（`IpcServerConfig`、`IpcServerAdapter::with_config` で調整可）:
  - **`hello_timeout`（既定 2 秒）**: 接続受理から有効な `Hello` が届くまでの上限。超過した接続は切断する（`HelloTimeout`、`Hello` 未完了のため `SourceDisconnected` は発行しない）。
  - **`idle_timeout`（既定 5 秒）**: `Hello` 後、**任意の種別のフレーム**（`Heartbeat` 含む）が届かない時間の上限。超過した接続は切断し `SourceDisconnected { reason: IdleTimeout }` を発行する。
  - **`max_connections`（既定 16）**: 同時接続数の上限。超過分の新規接続は即座に破棄する。
  - 切断理由は `AudioInputEvent::SourceDisconnected.reason`（`DisconnectReason`: `Eof` / `IoError` / `Protocol` / `HelloTimeout` / `IdleTimeout` / `Superseded` / `Shutdown`）で通知され、`kotoha-ipc-monitor` が表示する。
- **切断クリーンアップ (`SourceReset` / EOF)**:
  - クライアントはプラグイン停止時やボイスチャットサーバー切断時（`mumble_onServerDisconnected`）に `SourceReset` (`0x06`) を送信する。
  - サーバーは `SourceReset` 受信時または TCP/IPC EOF・I/O エラー検知時に（`conn_id` 所有権チェックのもとで）、該当接続の `source` に紐づくすべてのアクティブ話者・STT ストリームを安全に破棄・フラッシュする。

### 3.3 エンディアンとフレーミング（固定 8 バイトヘッダ）

- すべてのマルチバイト整数（`u16`, `u32`）および浮動小数点数（`f32`, IEEE 754 binary32）は **Little-Endian (LE)** でエンコードする。
- すべてのメッセージは以下の **固定 8 バイトヘッダ + 可変長ペイロード** で構成される。パディングは一切入れない（Packed layout）。

```text
Offset   Size  Field        Type    Description
0..2     2     magic        [u8;2]  固定値 [0x56, 0x54] (ASCII "VT")
2..3     1     version      u8      固定値 1 (プロトコルバージョン v1)
3..4     1     msg_type     u8      メッセージ種別 (0x00..0x06)
4..8     4     payload_len  u32 LE  直後に続くペイロードのバイト数 (0 ..= 65536)
8..8+N   N     payload      [u8;N]  N = payload_len (最大 65536 バイト)
```

#### ヘッダ検証ルール（デコーダ必須チェック）
1. `magic != [0x56, 0x54]` → プロトコルエラー（`InvalidMagic`）。即座に接続を切断する。
2. `version != 1` → プロトコルエラー（`UnsupportedVersion`）。即座に接続を切断する。
3. `payload_len > 65536` (`MAX_PAYLOAD_LEN = 65536`) → プロトコルエラー（`PayloadTooLarge`）。OOM 防止のためペイロードを割り当てず即座に接続を切断する。
4. `msg_type` が `0x00..=0x06` 以外 → プロトコルエラー（`UnknownMessageType`）。
5. 1 フレームの最大総バイト数は `8 + 65536 = 65544` バイトとする。

### 3.4 メッセージ種別とバイト単位ペイロード定義

#### `0x00` `Hello`（クライアント識別・ハンドシェイク）
- **最小長**: 5 バイト（`1 <= app_name_len <= 64` かつ `0 <= plugin_version_len <= 64`）、最大長: `132` バイト（仕様上限 `65536` 以下の固定上限）。
- **バイトレイアウト**:

```text
Offset      Size        Field               Type    Constraint
0..2        2           app_name_len        u16 LE  1 <= app_name_len <= 64
2..2+A      A           app_name_utf8       [u8;A]  有効な UTF-8 文字列、NUL('\0')禁止 (推奨: "mumble", "ts3", "mock")
2+A..4+A    2           plugin_version_len  u16 LE  0 <= plugin_version_len <= 64
4+A..4+A+V  V           plugin_version_utf8 [u8;V]  有効な UTF-8 文字列、NUL('\0')禁止 (例: "0.1.0")
```

- **厳密検証**:
  - `payload_len >= 5`（ヘッダ内長 4 バイト + `app_name_len >= 1`）
  - `4 + (app_name_len as usize) + (plugin_version_len as usize) == payload_len`（末尾の余剰バイト・切り詰めを禁止）
  - `1 <= app_name_len <= 64` かつ `plugin_version_len <= 64`（C++ エンコーダ・Rust デコーダ双方で完全一致）
  - `app_name_utf8` および `plugin_version_utf8` は有効な UTF-8 であり、**NUL バイト (`0x00`) を一切含まない**こと（他の有効な UTF-8 文字は許容し、Rust 側で `SpeakerKey.source` として利用）。

#### `0x01` `AudioFrame`（PCM 音声フレーム）
- **最小長**: 16 バイト（`payload_len == 16` の 0 サンプルフレームは構文上許容）、最大長: `65536` バイト。
- **バイトレイアウト**:

```text
Offset   Size  Field        Type     Constraint
0..4     4     user_id      u32 LE   ソース内で一意な話者 ID
4..8     4     seq          u32 LE   プラグイン単位の単調増加シーケンス番号 (0, 1, 2, ... wrap around可)
8..12    4     sample_rate  u32 LE   8000 <= sample_rate <= 192000 (Hz)
12..14   2     channels     u16 LE   1 <= channels <= 8
14..16   2     flags        u16 LE   bit 0 (0x0001) = IS_LOCAL_USER, bit 1..15 は予約 (必ず 0)
16..N    N-16  pcm_samples  [f32 LE] インターリーブ PCM サンプル列 (値域 [-1.0, 1.0])
```

- **厳密検証**:
  - `payload_len >= 16`（`payload_len == 16` の 0 サンプルフレームも構文上はデコード可能とするが、プラグイン実装は通常 1 サンプル以上を送信する）
  - `1 <= channels <= 8`
  - `8000 <= sample_rate <= 192000`
  - `(payload_len - 16) % (4 * channels) == 0`（インターリーブされた `f32` サンプル列のフレーム境界整合性）
  - 予約フラグ: `flags & 0xFFFE == 0`（bit 0 の `0x0001 = IS_LOCAL_USER` のみ有効。非ゼロの予約ビットは `InvalidFlags` として拒否し接続切断）。
  - PCM サンプル値検証: C++ エンコーダは送信前に必ず `std::isfinite(s)` を先に行い、`std::isfinite(s) ? std::clamp(s, -1.0f, 1.0f) : 0.0f` で無害化する（`NaN` を `std::clamp` に直接渡してはならない）。Rust デコーダは各 `f32` サンプルについて `sample.is_finite() && (-1.0..=1.0).contains(&sample)` を検証し、違反フレームは `InvalidPcmSample` として拒否し接続切断する。
- **チャンク分割と 8ch アライメント**:
  - 1 チャンクあたりの最大フロート数 `max_floats` は `channels` の整数倍になるよう切り捨てる：
    `max_floats = ((65536 - 16) / (4 * channels)) * channels`
    （例: `1ch..=7ch` → すべて `16380` floats [`16380` は 1〜7 で割り切れる]、`8ch` → `16376` floats）。
  - 1 つの音声コールバックで渡されたサンプル数が `max_floats` を超える場合、**Producer（またはリングバッファへの投入境界）で `max_floats` 単位に分割**して個別のスロットへ投入する。各チャンクがリングバッファ投入時に独自の単調増加 `seq` を受け取るため、ワイヤ上での `seq` と 1:1 に整合する。
- **`seq` の意味論・遅延初期化・回帰ガード**:
  - `seq` は送信側プラグインインスタンス全体で 0 から単調増加（`u32` wrapping add）するカウンタであり、**`SourceReset` を跨いでもリセットせず単調増加を維持する**（サーバーから切断・再接続してもカウント継続）。
  - リングバッファ投入試行時に `seq` が割り当てられるため、満杯（Drop-New）時でも欠番分の `seq` が消費され、後続の成功フレームにギャップが生じる。
  - **Rust 側 `expected_seq` の初期化と検証**:
    - 接続直後は `expected_seq: Option<u32> = None` とし、**その接続で最初に受信した `AudioFrame` の `seq` で `expected_seq = Some(frame.seq.wrapping_add(1))` と遅延初期化する**。これによりクライアント再接続時に `seq > 0` から始まっても偽のドロップ計上を行わない。
    - 2 フレーム目以降は `let delta = frame.seq.wrapping_sub(expected);` を計算する：
      - `delta == 0`: 正常連続（ドロップ 0）。
      - `1 <= delta < 0x8000_0000`: `delta` フレーム分のドロップとして統計に加算し、`expected_seq = Some(frame.seq.wrapping_add(1))` を更新。
      - `delta >= 0x8000_0000`（重複・順序逆転・極端な `seq` 回帰/リセット）: 約 42.9 億フレームの偽ドロップとして加算せず、**シーケンス異常（Sequence Anomaly / Reset）として警告ログを記録し `expected_seq = Some(frame.seq.wrapping_add(1))` へ再同期**する。
- **1 フレームの推奨サイズ**:
  - 48 kHz ステレオ 20 ms = `48000 * 0.02 * 2 * 4 = 7680` バイト（+16 バイト = 7696 バイト <= 65536 バイト）。

#### `0x02` `TalkingState`（発話状態通知）
- **固定長**: `payload_len == 5` バイト。
- **バイトレイアウト**:

```text
Offset  Size  Field    Type    Constraint
0..4    4     user_id  u32 LE  対象話者 ID
4..5    1     state    u8      0..=4 の列挙値
```

- **列挙値 `state`**:
  - `0` = `Passive`（非発話）
  - `1` = `Talking`（通常発話）
  - `2` = `Whispering`（ウィスパー発話）
  - `3` = `Shouting`（シャウト発話）
  - `4` = `TalkingMuted`（ミュート中発話検知）
  - `5..=255` = 不正値（デコーダは `InvalidTalkingState` として拒否）。

#### `0x03` `UserInfo`（話者表示名通知）
- **最小長**: `payload_len >= 4` バイト（`name_utf8` が空の場合 4 バイトも許容可。ただし空名はフォールバック表示名とする）。最大長 `65536` バイト。
- **バイトレイアウト**:

```text
Offset  Size  Field      Type     Constraint
0..4    4     user_id    u32 LE   対象話者 ID
4..N    N-4   name_utf8  [u8;N-4] 有効な UTF-8 文字列
```

- **厳密検証**: `payload_len >= 4`、`name_utf8` が有効な UTF-8 であり、**NUL バイト (`0x00`) を一切含まない**こと。

#### `0x04` `Heartbeat`（生存確認）
- **固定長**: `payload_len == 0` バイト。ペイロードなし（8 バイトヘッダのみ）。

#### `0x05` `UserRemoved`（話者退出通知）
- **固定長**: `payload_len == 4` バイト。
- **バイトレイアウト**:

```text
Offset  Size  Field    Type    Constraint
0..4    4     user_id  u32 LE  退出した話者 ID
```

#### `0x06` `SourceReset`（ソースリセット通知）
- **固定長**: `payload_len == 0` バイト。ペイロードなし（8 バイトヘッダのみ）。
- プラグインがボイスチャットサーバーから切断した際（`mumble_onServerDisconnected` 等）に送信し、Rust サーバー側に当該 `source` の全アクティブ話者および STT ストリームのクリーンアップを指示する。

### 3.5 メッセージ一覧サマリ

| `msg_type` | 名前 | 方向 | `payload_len` 制約 | 主な用途 |
| :--- | :--- | :--- | :--- | :--- |
| `0x00` | `Hello` | Plugin → Core | `5..=132` (`app_name: 1..=64`, `ver: 0..=64`) | アプリ名・プラグインバージョン通知 |
| `0x01` | `AudioFrame` | Plugin → Core | `>= 16` (`<= 65536`) | 音声 PCM フレーム送信 |
| `0x02` | `TalkingState` | Plugin → Core | `== 5` | 話者の発話状態変化通知 |
| `0x03` | `UserInfo` | Plugin → Core | `>= 4` (`<= 65536`) | 話者表示名の登録・更新 |
| `0x04` | `Heartbeat` | Plugin → Core | `== 0` | 1 秒周期キープアライブ |
| `0x05` | `UserRemoved` | Plugin → Core | `== 4` | 話者のチャンネル/サーバー退出通知 |
| `0x06` | `SourceReset` | Plugin → Core | `== 0` | サーバー切断時の全ストリーム解放 |

---

## 4. コンポーネント詳細設計 (Component Specifications)

### 4.1 C++20 プラグイン群 (`plugins/common`, `plugins/mumble`, 将来 `plugins/ts3`)

#### 共通基盤 (`plugins/common`)
- **言語・コンパイラ要件**: C++20、警告最大（MSVC `/W4 /WX` 相当または厳格警告、GCC/Clang `-Wall -Wextra -Wpedantic`）。例外・RTTI の音声スレッド内での使用を回避する。
- **`protocol.hpp/.cpp`**:
  - 第 3 章の仕様と完全に一致する LE エンコーダ / デコーダ。
  - `pcm_samples` はエンコード前に必ず `std::isfinite(s) ? std::clamp(s, -1.0f, 1.0f) : 0.0f` で無害化する（`NaN` を `std::clamp` へ直接渡さない）。
  - `Hello` の `app_name` は `1..=64` バイト、`plugin_version` は `0..=64` バイト、いずれも有効 UTF-8 かつ NUL バイト禁止。
- **`ring_buffer.hpp`（ロックフリー SPSC リングバッファ）**:
  - 固定長スロット配列（各スロットの最大 PCM フロート数は `16380` floats ＝ `(65536 - 16) / 4` 以下、リング容量例: 128〜256 スロット）を事前確保（起動時に 1 度だけ確保）し、音声コールバック内でのヒープ確保（`new`/`malloc`/`std::vector` リサイズ）・`std::mutex`・システムコールを **一切行わない**。
  - Single-Producer (音声コールバック) / Single-Consumer (IPC ワーカースレッド) 設計、`std::atomic` の `acquire`/`release` メモリオーダリングのみで構成。
  - **満杯時ポリシー（Drop-New 統一）**:
    - 満杯時ポリシーは **Drop-New** に一本化する。Producer は決して `read_idx` や未読スロットを書き換えない。
    - 音声コールバックが `max_floats = ((65536 - 16) / (4 * channels)) * channels` ごとに分割した各チャンクを `try_push` する際、まず単調増加カウンタ `seq`（`SourceReset` を跨いで維持）を 1 消費・割り当てする。
    - バッファが満杯であれば、その新規チャンクを即座に破棄して `dropped_count`（`std::atomic<uint64_t>`）をインクリメントして戻る。これにより音声スレッドは完全にロックフリー・非ブロッキングを保ち、次の成功チャンクに `seq` ギャップが生じて Rust 側が正確なドロップ数を検知できる。
- **`ipc_client.hpp/.cpp`**:
  - Windows（`CreateFileW` + `FILE_FLAG_OVERLAPPED` + `WriteFile` OVERLAPPED + `CancelIoEx` による Named Pipe、`ERROR_PIPE_BUSY` 時は `WaitNamedPipeW` 併用）および POSIX（`socket(AF_UNIX, SOCK_STREAM, 0)` + Linux `MSG_NOSIGNAL` / macOS `SO_NOSIGPIPE` + 非ブロッキングシャットダウン）の抽象化。
  - 制御イベントキュー（`TalkingState`, `UserInfo`, `UserRemoved`, `SourceReset`）は独立した `std::mutex` + `std::deque` で保持し、音声リングバッファとは完全分離する（`Hello` は接続確立時にワーカー自身が生成して最優先送信するため制御キューには入れない）。
  - **ワーカースレッドの待機ケイデンス（Bounded Wait）と厳密なドレイン順序**:
    1. 未接続時は条件変数 `wait_for` で **1000 ms** 待機しつつ再接続試行（シャットダウン通知時は即座に起床・離脱）。
    2. 接続成功直後、他のどのフレームよりも先に **`Hello` (`0x00`)** を 1 回送信する。
    3. 接続中ループでは以下の順でドレイン・送信する：
       - **(a) 非破壊的制御イベント先行**: 制御キューのうち `UserInfo`, `TalkingState` 等を先に送信してもよいが、**破壊的ライフサイクルイベント（`UserRemoved`, `SourceReset`）を送信する前に、必ず現在 `ring_buffer` に滞留している全音声フレームを先にドレイン・送信する**（`SourceReset` 前に直前までの発話音声が欠落・破棄されないようにするため）。実際には各反復で「非破壊的制御イベント → `ring_buffer` 全ドレイン → 破壊的制御イベント（`UserRemoved`/`SourceReset`）」の順で処理するのが最も安全で決定的である。
       - **(b) Heartbeat 送信**: 最終フレーム送信時刻（または最終 `Heartbeat`）から **1000 ms** 経過したら `Heartbeat` (`0x04`) を送信する。
       - **(c) Bounded Wait（2〜5 ms）**: `ring_buffer` および制御キューがともに空になったら、音声スレッドからの通知（システムコール禁止のため CV signal は使わない）を待たず、制御用 CV 上の `wait_for(2ms..5ms)` で短時間スリープする。フレームが 1 つでも存在する間はスリープせず即座に次フレームをドレインし続ける。これにより音声コールバックでのシステムコール 0 回・ビジースピン 0・最大遅延 <= 5 ms を同時に達成する。
    4. 書き込み失敗（`EPIPE` / `BROKEN_PIPE` 等）検知時はハンドル/ソケットを閉じ、1000 ms 後の再接続状態へ遷移する。
    5. **シャットダウン時順序**: `shutdown()` 呼び出し時は、`SourceReset` を制御キューに積み（接続中の場合）、残存音声フレームと `SourceReset` のドレイン/フラッシュ完了（または短時間の送信完了）を待ってから、進行中 I/O をキャンセルしワーカースレッドを `join()` する。

#### Mumble プラグイン (`plugins/mumble`)
- **使用 API**: Mumble Plugin API v1.0.x（`MumblePlugin_v_1_0_x.h`, `MumbleAPI_v_1_0_x.h`, `PluginComponents_v_1_0_x.h` を `plugins/mumble/third_party/mumble/` に同梱し、BSD-3-Clause `LICENSE` を明示）。
- **型・リンケージ・呼出規約**:
  - すべてのエクスポート関数は `extern "C"` ブロック内で `PLUGIN_EXPORT <ret> PLUGIN_CALLING_CONVENTION mumble_...` を用いて定義する。
  - ユーザー ID の型は `PluginComponents_v_1_0_x.h` 定義の `mumble_userid_t`（`typedef uint32_t mumble_userid_t;`）をそのまま使用し、`AudioFrame.user_id`（`u32`）へ損失なく格納する。
  - `mumbleAPI.getUserName` や `mumbleAPI.getAllUsers` 等が返却する Mumble 側確保メモリ（`const char*` / `mumble_userid_t*`）は、参照コピー完了後に必ず **`mumbleAPI.freeMemory(ownID, ptr)`** を呼んで解放する（`STATUS_OK` 時のみ確保される規約に準拠）。
- **音声スレッド不変条件 (`mumble_onAudioSourceFetched`)**:
  - 引数 `isSpeech == false` の場合は即座に `return false;` する（無音データの送信を省き CPU・IPC 帯域を節約）。
  - ローカルユーザー判定に用いる `localUserID` は制御スレッド（`mumble_onServerSynchronized` 等）と音声コールバック間で共有されるため、必ず **`std::atomic<uint32_t> local_user_id_`** と **`std::atomic<bool> has_local_user_id_`**（`memory_order_relaxed` または `acquire/release`）で共有し、ロック・データ競合をゼロにする。
  - `isSpeech == true` の場合のみ、`has_local_user_id_.load() && userID == local_user_id_.load()` なら `IS_LOCAL_USER` (`0x0001`) を付与し、前項のチャンク分割規約に従って SPSC リングバッファへ `try_push` する。
  - 音声データ自体は改変せず、**常に `false` を返却**する（Mumble 側の再生音声に一切干渉しない）。
- **制御コールバック**:
  - `mumble_init` / `mumble_shutdown`: IPC クライアント起動、`mumble_shutdown()` では `SourceReset` フラッシュ後にワーカースレッドを `join()`。
  - `mumble_onServerSynchronized`: `getLocalUserID` で取得した ID を `local_user_id_` / `has_local_user_id_` へ格納し、`getAllUsers` + `getUserName` で現在接続中ユーザー名を取得して `UserInfo` を制御キューへ投入（各ポインタは `freeMemory` で解放）。
  - `mumble_onServerDisconnected`: `has_local_user_id_.store(false)` とし、`SourceReset` を制御キューへ投入。
  - `mumble_onUserAdded` / `mumble_onUserRemoved`: 対応する IPC メッセージを制御キューへ投入。
  - `mumble_onUserTalkingStateChanged`: `Mumble_TalkingState` のうち **`MUMBLE_TS_INVALID` (`-1`) をフィルタリングして無視**し、有効値 `0..=4`（`MUMBLE_TS_PASSIVE`..`MUMBLE_TS_TALKING_MUTED`）のみを `TalkingState` (`0x02`) としてエンコードする。
- **テスト用成果物**:
  - `kotoha_mumble` 共有ライブラリ（`libkotoha_mumble.so` / `libkotoha_mumble.dylib` / `kotoha_mumble.dll`）
  - `kotoha_mock_client` CLI バイナリ（実 Mumble プロセスなしで任意の `Hello` / `UserInfo` / `TalkingState` / 合成正弦波・音声 `AudioFrame` / `Heartbeat` / `UserRemoved` / `SourceReset` を実 IPC で送信し、クロス言語 E2E テストに使用）。

#### 将来の TeamSpeak 3 プラグイン (`plugins/ts3` — Phase 4)
- TS3 Client Plugin SDK の `ts3plugin_onEditPlaybackVoiceDataEvent` / `ts3plugin_onEditPostProcessVoiceDataEvent` を使用。
- TS3 が提供する `short*` (`i16`) PCM サンプルをプラグイン側で `static_cast<float>(sample) / 32768.0f`（`[-1.0, 1.0]` クランプ）へ変換し、`plugins/common` の `AudioFrame` (`0x01`) として送信する。`app_name = "ts3"`。

---

### 4.2 Rust コア & エンジン層 (`crates/` — Clean / Hexagonal Architecture)

Rust コアは **Clean / Hexagonal (Onion) Architecture** に基づき、依存関係が常に**外側から内側（Outer → Inner）のみ**へ向かう厳格なレイヤー構造を採用する。

```text
[Presentation] ──▶ [Application] ──▶ [Domain] ◀── [Infrastructure]
 (CLI / Tauri)     (Use Cases)       (Entities,    (IPC, DSP, Sherpa,
                                      Ports, Pure)  ct2rs, HTTP, Storage)
```

#### レイヤー責務とモジュール分割 (`crates/kotoha-core/src/` & Workspace)

| レイヤー | パス / クレート | 依存許可範囲 | 主な責務・構成要素 |
| :--- | :--- | :--- | :--- |
| **Domain (最内層)** | `kotoha-core/src/domain/` | 純粋 Rust (`serde`, `thiserror` 等のみ。`tokio`/`reqwest`/`interprocess`/`tauri` 禁止) | **Entities / Value Objects**: `SpeakerKey`, `LanguageCode`, `AudioChunk`, `UtteranceId`, `TranscriptEvent`, `TranslationRequest`, `TranslationResult`, `TranslateContext`, `SubtitleEntry`, `GlossaryProfile`, `HotwordsConfig`, `ModelProfile`, `ComputeDevice`, `TranslationProviderConfig`<br>**Ports (Traits)**: `SttEngine`, `SttStream`, `Translator`, `AudioInputSource`, `SubtitleSink`, `SettingsRepository`, `GlossaryRepository`<br>**Pure Logic**: Pre-filter 判定、話者カラーハッシュ、Context Window 管理、用語辞書最長一致プレースホルダ保護/復元 |
| **Application** | `kotoha-core/src/application/` | `domain` のみ | `PipelineService`（前処理・話者別 STT ストリーム制御・Pre-filter・文脈/用語辞書注入・Semaphore 並列翻訳・`utterance_id` 順序保証・`SubtitleSink` 通知）<br>`SourceSessionManager`（`conn_id` 所有権管理と切断クリーンアップ）<br>`TranslatorManager`（設定変更時の翻訳プロバイダ／モデルプロファイル実行時ホットスワップ）<br>`GlossaryManager`（複数 TOML 用語辞書の読込・優先マージ・STT Hotwords / 翻訳辞書同期） |
| **Infrastructure** | `kotoha-core/src/infrastructure/`<br>`kotoha-protocol`<br>`kotoha-stt-sherpa` (P2)<br>`kotoha-translate-local` (P2)<br>`kotoha-translate-http` (P2) | `domain`, `application`, 外部 I/O クレート | `IpcServerAdapter`（`tokio` + `interprocess` + `kotoha-protocol`、`AudioInputSource` 実装）<br>`AudioPreprocessor`（任意 ch → モノラル平均ダウンミックス、8k..192kHz → 16 kHz `f32` リサンプラ）<br>`MockSttAdapter` / `MockTranslatorAdapter`（Phase 1 検証用）<br>`TomlGlossaryRepository`（`glossaries/*.toml` 読込）<br>Phase 2 外部アダプタ群（`sherpa-onnx`, `ct2rs` + モデルダウンローダ, HTTP API 群）<br>`JsonSettingsRepository` / OS キーチェーン連携 |
| **Presentation (最外層)** | `kotoha-core/src/bin/kotoha_ipc_monitor.rs` (P1)<br>`app/src-tauri` (P3) + `app/ui` | 全内側レイヤー（DI 組み立て担当） | `kotoha-ipc-monitor` CLI（受信統計・RMS・ドロップ数・パイプライン出力）<br>Tauri v2 Commands / Event Bridge (`SubtitleSink` 実装) / System Tray / Overlay UI |

#### Workspace クレート構成
- `crates/kotoha-protocol`:
  - 軽量依存（`thiserror` 等最小限）で、IPC v1 バイナリコーデック・フレーム検証・エンドポイント解決を提供。
- `crates/kotoha-core`:
  - `domain/` + `application/` + `infrastructure/`（Phase 1 の `IpcServerAdapter`, `AudioPreprocessor`, `TomlGlossaryRepository` 基盤, Mock アダプタ, `kotoha-ipc-monitor` バイナリ）を収容。
- `crates/kotoha-stt-sherpa`（Phase 2）:
  - `sherpa-onnx`（CPU 軽量ストリーミングモデル、例: Zipformer / Paraformer 等）を用いた `SttEngine` / `SttStream` アダプタ。
  - **Hotwords (Context Biasing)** による用語ブースト（Transducer では `decoding_method = "modified_beam_search"` 使用、`hotwords_score` 設定対応）。
  - 話者（`SpeakerKey`）ごとに独立したストリームを保持し、無音タイムアウト・`UserRemoved`・`SourceReset` で確実に解放する。
- `crates/kotoha-translate-local`（Phase 2）:
  - `ct2rs` (CTranslate2) ベースの `Translator` アダプタ、ハードウェア判定・モデルカタログ管理・SHA-256 検証付きダウンローダ。
- `crates/kotoha-translate-http`（Phase 2）:
  - `reqwest` ベースの HTTP 翻訳プロバイダ群（OpenAI 互換 LLM / DeepL / Google Cloud Translation v2・v3 / GAS Webhook）。

#### Domain Entities・Value Objects & Ports (`crates/kotoha-core/src/domain/`)

```rust
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpeakerKey {
    pub source: String,
    pub user_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LanguageCode(pub String); // ISO 639-1 / BCP-47 e.g. "en", "ja", "en-US"

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct UtteranceId(pub u64);

#[derive(Debug, Clone, PartialEq)]
pub struct AudioChunk {
    pub speaker_key: SpeakerKey,
    pub samples_16k_mono: Vec<f32>,
    pub timestamp_ms: u64,
    pub is_local_user: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptEvent {
    pub utterance_id: UtteranceId,
    pub speaker_key: SpeakerKey,
    pub text: String,
    pub is_final: bool,
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubtitleEntry {
    pub utterance_id: UtteranceId,
    pub speaker_key: SpeakerKey,
    pub speaker_name: String,
    pub speaker_color: String,
    pub src_text: String,
    pub translated_text: Option<String>, // None while Partial/Final STT, Some after translation
    pub stage: SubtitleStage, // Interim / FinalPendingTranslation / Translated / TranslationFailed
    pub failure_reason: Option<String>, // TranslationFailed 時のみ。固定の利用者向け文言（serde で None は省略）
    pub timestamp_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlossaryProfile {
    pub name: String,
    pub src_lang: LanguageCode,
    pub tgt_lang: LanguageCode,
    pub terms: std::collections::BTreeMap<String, String>, // raw term -> fixed target translation
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct HotwordsConfig {
    pub enabled: bool,
    pub hotwords_score: f32, // sherpa-onnx context biasing score (default e.g. 1.5)
}

#[derive(Debug, Clone, Default)]
pub struct SttSessionOptions {
    pub hotwords: Vec<String>, // active merged glossary [terms] keys
    pub hotwords_score: f32,
}

pub trait SttStream: Send {
    fn accept_audio(&mut self, samples_16k_mono: &[f32], timestamp_ms: u64) -> Result<Vec<TranscriptEvent>, SttError>;
    fn flush(&mut self, timestamp_ms: u64) -> Result<Vec<TranscriptEvent>, SttError>;
}

pub trait SttEngine: Send + Sync {
    fn name(&self) -> &'static str;
    fn create_stream(
        &self,
        speaker_key: SpeakerKey,
        src_lang: &LanguageCode,
        options: &SttSessionOptions,
    ) -> Result<Box<dyn SttStream>, SttError>;
}

#[async_trait::async_trait]
pub trait Translator: Send + Sync {
    fn name(&self) -> &'static str;
    async fn translate(&self, req: TranslationRequest) -> Result<TranslationResult, TranslateError>;
}

#[async_trait::async_trait]
pub trait AudioInputSource: Send + Sync {
    async fn run(&self, tx: tokio::sync::mpsc::Sender<SourceEvent>) -> Result<(), SourceError>;
}

#[async_trait::async_trait]
pub trait SubtitleSink: Send + Sync {
    async fn emit_subtitle(&self, entry: SubtitleEntry) -> Result<(), SinkError>;
    async fn clear_source(&self, source: &str) -> Result<(), SinkError>;
}

#[async_trait::async_trait]
pub trait SettingsRepository: Send + Sync {
    async fn load(&self) -> Result<AppSettings, SettingsError>;
    async fn save(&self, settings: &AppSettings) -> Result<(), SettingsError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlossaryProfileMeta {
    pub id: String,
    pub name: String,
    pub src_lang: LanguageCode,
    pub tgt_lang: LanguageCode,
    pub term_count: usize,
}

#[async_trait::async_trait]
pub trait GlossaryRepository: Send + Sync {
    async fn list_profiles(&self) -> Result<Vec<GlossaryProfileMeta>, GlossaryError>;
    async fn load_profile(&self, id: &str) -> Result<GlossaryProfile, GlossaryError>;
    async fn save_profile(&self, id: &str, profile: &GlossaryProfile) -> Result<(), GlossaryError>;
}
```

#### Application オーケストレーション (`PipelineService` & `TranslatorManager` & `GlossaryManager`)
1. **Pre-filter（Domain 純粋ルール）**:
   - Partial 認識中は翻訳を発火せず（デフォルトは `is_final == true` 時のみ翻訳して計算コストと API 課金を節約）、`SubtitleSink` へ即座に `Partial STT` を送信する。
   - `is_final == true` のテキストから空白トリム後の空文字、極端な短発話・記号のみ、および設定されたフィラー語（例: `"uh"`, `"um"`, `"ah"`, `"hmm"` 単体）を除外し、不要な翻訳リクエストを抑制する。
2. **Context Manager（`TranslateContext`）**:
   - チャンネル/セッション単位で直近 **5〜10 発話**（設定可能、デフォルト 8 発話）の「話者名 + 原文 + 訳文」をリング履歴として保持。
   - `GlossaryManager` が優先順位順にマージした汎用カスタム用語辞書（`glossary`）を `TranslateContext` 経由で翻訳層へ注入する。
3. **並列翻訳・順序保証 (`PipelineService`)**:
   - 翻訳処理は `tokio::sync::Semaphore` で同時実行数上限（例: 4）を制御しつつ非同期並列実行する。
   - UI への翻訳結果反映は `utterance_id` をキーとして紐付けるため、翻訳完了順が前後しても対応する字幕カードが正確に更新され、表示順序の破綻を防ぐ。
   - **エラー方針（単一障害でパイプラインを止めない）**: Domain の純粋関数 `CoreError::scope() -> ErrorScope`（`Chunk < Utterance < Speaker < Fatal`）でエラーの影響範囲を分類し、呼び出し箇所ごとの上限（`ErrorScope::at_most`）で丸めた範囲だけを破棄して処理を継続する。`run()` を終了させるのは `Fatal`（パイプライン自身の不変条件違反。例: 翻訳 Semaphore の close）のみで、入力チャンネルの close は `Ok(())` での正常終了とする。

     | 発生箇所 | 上限スコープ | 回復動作 |
     |---|---|---|
     | `ResampleFn`（不正チャンク） | `Chunk` | 当該チャンクを破棄 |
     | `SttEngine::create_stream` / `SttStream::accept_audio` | `Speaker` | `Chunk` ならチャンクのみ破棄、それ以外は当該話者ストリームを破棄し次チャンクで再生成 |
     | `SttStream::flush` | `Speaker` | 当該話者ストリームを破棄 |
     | `Translator::translate`（エラー・panic とも） | `Utterance` | `SubtitleStage::TranslationFailed`（JSON: `"translation_failed"`、`failure_reason` に固定文言）を通知。Context Window には記録しない |
     | `SubtitleSink::emit` / `clear_source` | `Utterance` | 当該更新のみ欠落（UI 未接続でも継続） |
     | 翻訳タスクの `JoinError` | `Utterance` | 計上して継続 |

     - `failure_reason` はアダプタのエラーメッセージ（プロバイダ応答本文を含み得る）を転送せず、`CoreError::translation_failure_reason()` の固定文言（例: `"translation provider error"`、panic 時は `"translation engine crashed"`）のみを UI へ渡す。
     - ロギング依存（`tracing` / `log`）は現時点で導入せず、非致命エラーは `PipelineStats`（`translation_failures` / `stt_errors` / `stt_stream_resets` / `dropped_chunks` / `sink_errors` / `task_panics`）に計上する。`PipelineService::stats_handle()` で `run()` 実行中もロックフリーに参照できる。
     - **リトライ / バックオフ**: パイプラインは翻訳を再試行しない（リアルタイム字幕では遅延した再試行より次の発話を優先する）。再試行が必要な場合は翻訳アダプタ（`kotoha-translate-http`）側の責務とする。
     - 話者別 STT ワーカー・上限付き翻訳キューへの再構成（#27）後も、同じ `ErrorScope` 分類と `PipelineStatsHandle`（`Clone` 可能）を各ワーカーで共有する前提とする。
4. **実行時ホットスワップ (`TranslatorManager` / `GlossaryManager`)**:
   - `TranslatorManager` は現在の `Arc<dyn Translator>` を `tokio::sync::RwLock`（または `arc_swap`）で保持し、ユーザーが設定画面でプロバイダや `ModelProfile` を切り替えても、進行中の IPC 接続や話者 STT ストリームを切断せずに即座に後続発話から新エンジンへ切り替えられる。
   - 用語辞書プロファイルや `hotwords_score` が更新された場合は、`GlossaryManager` がマージ済み `[terms]` を再計算し、以後の翻訳リクエストおよび STT ストリームへ即時反映する。

#### 汎用カスタム用語辞書 (Generic TOML Glossary) — STT Hotwords & 翻訳の単一ソース設計

1. **設計方針（ドメイン非依存 & Non-Goals）**:
   - **ドメイン非依存 (Domain-Agnostic)**: Kotoha コア自体は特定のゲーム・作品・業界に固有の用語知識を一切ハードコードしない。汎用的な TOML 用語辞書ファイルを外部から読み込む仕組みのみを提供する。
   - **Single Source of Truth**: 単一の `[terms]` キー・バリューマップが、**STT Hotwords（認識時コンテキストバイアス）** と **翻訳時の固定訳語マッピング** の双方を同時に駆動する。
   - **Explicit Non-Goals（意図的に採用しない機能）**:
     - **STT 後段の文字起こし補正 LLM を挿入しない**: 音声認識の直後に誤認識修正専用の LLM パスを追加するとレイテンシと計算/API コストが増大するため行わない。
     - **手動の後段誤認識置換テーブル (`[stt_corrections]` 等) を設けない**: 初期設計では STT の文字列後置換ルールを持たず、認識精度向上は STT Hotwords による認識段階でのブーストに一本化する。
     - **読み仮名・発音記号の必須化をしない**: 辞書キーには生の綴りをそのまま記述する（ただし略語・英字読み等で認識を助けたい場合に、ユーザー自身が任意で `"M W D" = "MWD"` のようにスペース区切り表記をキーとして記述することは許容する）。

2. **TOML 用語辞書仕様 (`glossaries/*.toml`)**:
   - アプリ設定ディレクトリ配下の `glossaries/*.toml` 等に複数のプロファイルファイルを配置でき、ユーザーは複数のプロファイルを選択・有効化し、**優先順位順（先頭から順にベース適用し、後続プロファイルが同一キーを上書きする「後勝ち」方式、または UI リスト下位→上位の順で上書き）にマージ**して使用できる（`src_lang` / `tgt_lang` が現在の翻訳ペアと一致するプロファイルを自動選択またはフィルタ可能）。
   - スキーマ例:

     ```toml
     name = "My Profile"
     src_lang = "en"
     tgt_lang = "ja"

     [terms]
     "Ferox" = "Ferox"
     "Logi" = "ロジ"
     "Anchor" = "アンカー"
     "M W D" = "MWD"
     ```

3. **レイヤー別統合仕様（STT 層 & 翻訳層）**:
   - **STT 層（`sherpa-onnx` Hotwords / Context Biasing）**:
     - 有効化・マージ済み `[terms]` の**すべてのキー**を `SttSessionOptions.hotwords` として `sherpa-onnx` の Hotwords に投入する。
     - Transducer モデル（例: Zipformer）で Hotwords 機能を有効にするため、`decoding_method = "modified_beam_search"` を指定する。
     - 一般単語（例: `"Anchor"` 等）への過剰バイアス・誤爆（false-positive over-triggering）を抑制調整できるよう、設定項目として **`hotwords_score`**（デフォルトブーストスコア）を公開する。
   - **翻訳層（`Translator` Trait / `TranslateContext`）**:
     - 有効化・マージ済みの用語辞書（`glossary` マップ）を `TranslateContext` に格納して `Translator::translate` へ渡す。
     - **Local NMT (`ct2rs` + FuguMT / OPUS-MT / NLLB-200) および専用翻訳 API (DeepL / Google Cloud / GAS)**:
       - 翻訳呼び出し前に、原文中の登録キーを**最長一致優先（longest-match-first）かつ単語境界（word-boundary）を考慮**して検索し、一時プレースホルダ（例: `__TERM_0__`, `__TERM_1__`）へ置換・保護する。
       - 翻訳完了後、訳文中の各プレースホルダを対応する固定訳語（`[terms]` の値）へ復元する（翻訳モデルや外部 API がプレースホルダの空白・大文字小文字をわずかに変化させた場合の正規表現フォールバック復元も検討する）。
     - **LLM API (OpenAI 互換 / Gemini / Ollama / LM Studio)**:
       - アクティブな用語辞書テーブル（`原文キー -> 固定訳語`）をシステムプロンプトへ注入し、前後の会話履歴（5〜10 発話）とともに自然な文法の中で指定訳語を適用させる。

#### 翻訳エンジン Port・環境別モデルカタログ・プロバイダ設定スキーマ

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextTurn {
    pub speaker_name: String,
    pub src_text: String,
    pub translated_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GlossaryEntry {
    pub term: String,
    pub translation: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TranslateContext {
    pub recent_turns: Vec<ContextTurn>, // 5..=10 turns (configurable)
    pub glossary: Vec<GlossaryEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationRequest {
    pub utterance_id: UtteranceId,
    pub speaker_name: String,
    pub text: String,
    pub src_lang: LanguageCode,
    pub tgt_lang: LanguageCode,
    pub context: TranslateContext, // recent 5..=10 turns + glossary
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationResult {
    pub utterance_id: UtteranceId,
    pub translated_text: String,
    pub provider_name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComputeDevice {
    Auto,  // CUDA/Metal 検出時は優先利用し、無ければ CPU へフォールバック
    Cpu,
    Cuda,
    Metal,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelProfile {
    pub model_id: String,
    pub backend: String,              // 例: "ctranslate2", "openai_compatible"
    pub supported_langs: Vec<String>, // 例: ["en->ja"] / ["multilingual"]
    pub recommended_device: ComputeDevice,
    pub size_mb: u32,
    pub license: String,              // 例: "CC-BY-SA-4.0", "CC-BY-4.0", "CC-BY-NC-4.0"
    pub sha256: String,
    pub download_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TranslationProviderConfig {
    /// Engine 1 (Recommended Default): 無料・完全オフライン・モデルプロファイル選択式 Local NMT
    LocalNmt {
        model_id: String,        // 例: "fugumt-en-ja-int8", "opus-mt-en-ja-int8", "nllb-200-distilled-600m-int8"
        device: ComputeDevice,   // Auto | Cpu | Cuda | Metal
        compute_type: String,    // 例: "int8" / "float16" / "default"
        num_threads: usize,      // 0 = auto
    },
    /// Engine 2: OpenAI 互換 LLM API (クラウド LLM / ローカル Ollama・LM Studio + GPU/Apple Silicon)
    OpenAiCompatible {
        base_url: String,        // 例: "https://api.openai.com/v1" / "http://localhost:11434/v1"
        model: String,           // 例: "gpt-4o-mini" / "gemini-2.5-flash" / "qwen2.5:7b"
        api_key_ref: String,     // OS キーチェーン参照 (ローカル Ollama 等は空許容)
        context_window: usize,   // 5..=10 (default 8)
    },
    /// Engine 3a: DeepL API (Free / Pro)
    DeepL {
        plan: DeepLPlan,         // Free | Pro
        api_key_ref: String,
        formality: Option<String>,
    },
    /// Engine 3b: Google Cloud Translation API (公式 v2 / v3, 月間無料枠あり)
    GoogleCloud {
        project_id: Option<String>, // v3 で使用
        api_version: GoogleTranslateApiVersion, // V2 | V3
        api_key_ref: String,
    },
    /// Engine 3c: GAS (Google Apps Script) / カスタム HTTP プロキシ
    GasWebhook {
        endpoint_url: String,           // ユーザー自身がデプロイした GAS Web App URL
        bearer_token_ref: Option<String>,
    },
}
```

#### ローカル翻訳モデルプロファイル選択設計（CPU / GPU 環境別カタログ）

Kotoha は「ローカル NMT」を単一の固定モデルとして扱わず、ユーザーの計算環境（CPU のみ vs NVIDIA CUDA GPU / Apple Silicon Metal GPU）と言語ペアに応じて **`ModelProfile` 単位で選択・ダウンロード・切替** できるカタログを提供する。

| プロファイル ID (`model_id`) | 種別・バックエンド | 推奨環境 (`ComputeDevice`) | サイズ目安 | ライセンス | 用途・特徴 |
| :--- | :--- | :--- | :--- | :--- | :--- |
| `fugumt-en-ja-int8` | 軽量 NMT (`ctranslate2`) | **`Cpu` (標準推奨)** | ~200 MB | **CC-BY-SA-4.0** | GPU 非搭載 PC でも超低遅延（数十 ms）な英日デフォルトモデル |
| `opus-mt-en-ja-int8` | 軽量 NMT (`ctranslate2`) | **`Cpu`** | ~150 MB | **CC-BY-4.0** | Helsinki-NLP 系軽量モデル。商用制限が緩やかな代替選択肢 |
| `nllb-200-distilled-600m-int8` | 多言語 NMT (`ctranslate2`) | **`Auto` (`Cpu` / `Cuda`)** | ~650 MB | **CC-BY-NC-4.0 (非商用限定)** | 200 言語対応の中量級モデル（要非商用注意表示） |
| `nllb-200-1.3b-fp16` | 高精度多言語 NMT (`ctranslate2`) | **`Cuda`** (VRAM 4GB+) | ~2.6 GB | **CC-BY-NC-4.0 (非商用限定)** | GPU 環境向け高精度多言語 NMT（要非商用注意表示） |
| ローカル LLM（例: `qwen2.5:7b` / `gemma2:9b` 等） | `OpenAiCompatible` (`Ollama` / `LM Studio`) | **`Cuda` / `Metal` (Apple Silicon)** | 4〜8 GB | モデル毎 (Apache-2.0 等) | 文脈 5〜10 発話＋用語辞書を活かした高精度・口語翻訳（`localhost:11434/v1` 等へ接続） |

#### 全翻訳プロバイダ比較と非公式エンドポイント非採用方針

| カテゴリ / プロバイダ | 実装技術 / 接続先 | 特徴・強み | 必要な外部要素 / モデル・API |
| :--- | :--- | :--- | :--- |
| **1. Local NMT (モデル選択式・推奨デフォルト)** | `ct2rs` (`kotoha-translate-local`) | **完全無料・オフライン・低遅延・プライバシー保護**。CPU 軽量から GPU 多言語まで選択可能 | 初回ウィザード／設定から `ModelProfile` を選択し自動 DL＋**SHA-256 検証** |
| **2a. OpenAI 互換 LLM (クラウド)** | `reqwest` (`kotoha-translate-http`) | 会話履歴（5〜10 発話）＋用語辞書で文脈・スラングを自然翻訳 | OpenAI / Gemini (OpenAI 互換) の API キー |
| **2b. OpenAI 互換 LLM (ローカル GPU/Mac)** | `reqwest` (`http://localhost:11434/v1` 等) | GPU / Apple Silicon 上で完全オフラインかつ文脈対応 LLM 翻訳 | ユーザー環境上の Ollama / LM Studio |
| **3a. DeepL API** | `reqwest` (`api-free.deepl.com` / `api.deepl.com`) | 高品質な定型翻訳・Free 枠（月 50 万文字）あり | DeepL API Key |
| **3b. Google Cloud Translation API** | `reqwest` (公式 REST API v2 / v3) | 多言語網羅・高可用・Google Cloud 無料枠（月 50 万文字相当）活用可 | Google Cloud API Key / 認証情報 |
| **3c. GAS / カスタム HTTP プロキシ** | `reqwest` (POST JSON `{"text","source","target"}` → `{"translatedText"}`) | ユーザーが自身でデプロイした Google Apps Script (`LanguageApp.translate()`) URL 等を利用し、アプリ側でのスクレイピング無しで無料利用可能 | ユーザー管理の GAS Web App URL（任意で Bearer Token） |

> [!WARNING]
> **非公式 Google Translate Web エンドポイント（例: `translate.googleapis.com/translate_a/single` 等）の非採用方針**:
> Kotoha は非公式の Google Translate Web スクレイピングエンドポイントを組み込みプロバイダとして**一切搭載しない**。理由は以下の 3 点である：
> 1. **利用規約 (ToS) 上のリスク**: 非公式エンドポイントの自動スクレイピングは利用規約上の曖昧さ・違反リスクを伴う。
> 2. **レート制限・CAPTCHA による実用不能化**: リアルタイム音声チャットでは数秒ごとに連続リクエストが発生するため、厳格なレート制限 (`HTTP 429`) や CAPTCHA チャレンジを容易に誘発し会話中に翻訳が停止する。
> 3. **脆弱な非公開スキーマ**: 文書化されていないレスポンス形式に依存するため、Google 側の変更で突如破綻する。
> 無料で Google 翻訳品質を利用したいユーザーには、**公式 Google Cloud Translation API（月間無料枠）** またはユーザー自身の責任でデプロイする **GAS (`LanguageApp.translate()`) アダプタ** を公式サポート経路として提供する。

> [!IMPORTANT]
> **モデルライセンス分離方針（重要）**:
> - Kotoha 本体のソースコードおよびバイナリは **MIT License** で提供する。
> - 音声認識・ローカル NMT の学習済みモデルはアプリ本体に静的に埋め込まず（バンドルせず）、初回起動ウィザード／設定画面で公式またはミラー URL からオンデマンドダウンロード（SHA-256 検証付き）する。
> - UI・ドキュメント上では使用するモデルごとにライセンスと利用条件を明確に表示・同意確認する：
>   - **FuguMT (en-ja CTranslate2 変換版)**: **CC-BY-SA-4.0**（商用利用可・表示/継承条件あり）
>   - **OPUS-MT (Helsinki-NLP)**: **CC-BY-4.0**（商用利用可・帰属表示あり）
>   - **NLLB-200 (Meta)**: **CC-BY-NC-4.0（非商用限定: Non-Commercial）** — ユーザーが任意に選択するオプションモデルとして明示的な非商用注意書きを表示し、MIT 本体ライセンスへの混入・誤認を防ぐ。
>   - **sherpa-onnx**: Apache-2.0（ライブラリ）/ 各モデル固有ライセンス（Apache-2.0 / MIT / CC-BY 等）をモデルカタログメタデータで明示する。

---

### 4.3 UI レイヤー (`app/ui` & `app/src-tauri`)

- **技術スタック**: Tauri v2 (Rust バックエンド) + React 19 + TypeScript + Vite。
- **3 つの主要画面 / コンポーネント**:
  1. **字幕オーバーレイウィンドウ (`Overlay`)**:
     - 透過・フレームレス・タスクバー非表示・最前面（Always-on-Top）・マウスイベント透過（クリックスルー: Tauri `set_ignore_cursor_events(true)`）。
     - **3 段階ライブ更新**:
       - Phase A: `Partial STT`（認識中テキストを淡色・イタリック等で即座に表示）
       - Phase B: `Final STT`（確定原文へ更新）
       - Phase C: `Translated`（`utterance_id` 一致の訳文をメイン表示し、任意で原文を小さく併記）
       - Phase C（失敗）: `TranslationFailed`（`stage == "translation_failed"`。確定原文を残したまま「翻訳失敗」と判別できる表示にし、`failure_reason` を任意で併記）
     - **話者カラー自動割当**: `SpeakerKey` のハッシュ等により視認性の高いパレットから自動割当。
     - **位置・サイズ変更**: 通常時は完全クリックスルー。システムトレイ（またはグローバルショートカット）から「位置ロック解除」すると一時的にドラッグ＆リサイズ可能になり、再ロックで即座にクリックスルーへ戻る。
  2. **システムトレイ (`System Tray`)**:
     - 常駐アイコンとコンテキストメニュー：「字幕表示 ON/OFF」「オーバーレイ位置ロック解除/固定」「接続状態表示 (IPC 接続元・ドロップ統計)」「設定を開く」「終了」。
  3. **設定 & 初回セットアップウィザード (`Settings` & `Wizard`)**:
     - 初回起動ウィザード:
       1. Mumble プラグイン（`include_bytes!` 等で Tauri バイナリ内に同梱した各 OS 用ビルド済みプラグイン）の自動配置先検出＆ワンクリックインストール。
       2. STT モデルおよびユーザー環境（CPU / GPU CUDA / Apple Silicon Metal）に応じた Local NMT `ModelProfile` の選択・ダウンロード・SHA-256 チェックサム検証・展開進捗表示。
       3. IPC `Heartbeat` 自動検知による「接続確認（グリーンチェック）」。
     - 設定画面:
       - `src_lang` / `tgt_lang` 選択。
       - 翻訳プロバイダ切替（Local NMT [CPU/GPU 環境別 `ModelProfile` & `ComputeDevice` 選択] / OpenAI 互換 LLM [クラウド・Ollama・LM Studio] / DeepL / Google Cloud Translation / ユーザー独自 GAS Webhook）＋ API キーの安全保存（OS 資格情報ストア連携、設定ファイル平文記載禁止）。
       - 汎用 TOML 用語辞書（`glossaries/*.toml` のインポート・有効化・優先順位並び替え・編集）、STT **`hotwords_score`** スライダー調整、コンテキスト履歴数（5〜10）、フォントサイズ・透過率・表示行数・フェードアウト秒数。

#### OS 別制約と実運用上の回避策（必須設計事項）

1. **Linux Wayland vs X11 / XWayland 制約**:
   - Wayland コンポジタ（GNOME Mutter / KDE KWin / Sway 等）ではセキュリティモデル上、ウィンドウが自身のスクリーン座標を自由配置することや、汎用的な `always-on-top` + `click-through` 制御が制限される場合がある。
   - 対策:
     - X11 および XWayland 環境（`GDK_BACKEND=x11` または Wayland 上の XWayland フォールバックオプション）を公式サポート動作モードとして明記する。
     - Tauri v2 の透過設定（`transparent: true`）および Wayland 上での透過・クリックスルー挙動（`gtk-layer-shell` 検討または X11/XWayland 起動ガイド）を実機検証し、Wayland 環境で透過やクリックスルーが効かない場合のフォールバック（コンパクトフローティングウィンドウ／XWayland 起動フラグ案内）を UI に表示する。
2. **Linux Flatpak / Snap 版 Mumble へのプラグイン配置**:
   - ネイティブ版 Mumble は `~/.local/share/Mumble/Mumble/Plugins/` を参照するが、**Flatpak 版 Mumble**（`info.mumble.Mumble`）はサンドボックス内の `~/.var/app/info.mumble.Mumble/data/Mumble/Mumble/Plugins/` を参照し、さらに Flatpak サンドボックス境界外の `$XDG_RUNTIME_DIR/kotoha_v1.sock` へアクセスできないケースがある。
   - 対策:
     - セットアップウィザードでネイティブパスと Flatpak パスの双方をスキャンし、検出された環境へ配置する。
     - Flatpak 版が検出された場合は、Flatpak 権限設定のガイドをウィザード上で表示する（例: 単一ファイルの `--filesystem=xdg-run/kotoha_v1.sock` バインドマウントはサーバー再起動でソケットが再作成されると古い inode が陳腐化する可能性があるため、将来的には専用ディレクトリ `$XDG_RUNTIME_DIR/kotoha/kotoha_v1.sock` と `--filesystem=xdg-run/kotoha` への移行や、現状での設定手順・注意点も案内する）。
3. **Windows プラグイン配置先 & ロック中 DLL のアップデートフロー**:
   - **配置先検出**: `%AppData%\Mumble\Mumble\Plugins\`（標準/インストーラ版）および `%LocalAppData%\Mumble\Mumble\Plugins\`（環境依存・フォールバック）をスキャンして配置する。
   - **ロック中 DLL 対策**: Windows では Mumble が `kotoha_mumble.dll` を `LoadLibrary` 中はファイルロックがかかり、上書き削除・置換ができない。
   - ウィザード／アップデータはまず上書きを試み、`ERROR_SHARING_VIOLATION` 等で失敗した場合は Mumble のプロセス稼働をユーザーに通知して再起動を促すか、`.pending` 一時保存・遅延置換（または Mumble 終了検知後の自動適用）フローを提供する。
4. **macOS 透過オーバーレイ & 権限・プラグイン配置**:
   - プラグイン配置先: `~/Library/Application Support/Mumble/Mumble/Plugins/`。
   - macOS のフルスクリーンゲーム上へのオーバーレイ表示は `NSWindowCollectionBehaviorCanJoinAllSpaces | FullScreenAuxiliary` 相当の設定が必要。App Sandbox は使わず Hardened Runtime + Notarization（Phase 4）を前提とする。

---

## 5. リポジトリ構成 (Repository Layout)

```text
kotoha/
├── DESIGN.md                        # 本設計仕様書 (Single Source of Truth)
├── README.md                        # プロジェクト概要・ビルド/テスト手順
├── LICENSE                          # MIT License (Copyright (c) 2026 4moda)
├── Cargo.toml                       # Rust Workspace ルート定義
├── .gitignore
├── .editorconfig
├── .clang-format
├── .clang-tidy
│
├── plugins/                         # C++20 ボイスチャットプラグイン群 (CMake)
│   ├── CMakeLists.txt
│   ├── common/                      # IPC v1, SPSC RingBuffer, OS Native IPC Client
│   │   ├── CMakeLists.txt
│   │   ├── include/kotoha/
│   │   │   ├── protocol.hpp
│   │   │   ├── ring_buffer.hpp
│   │   │   └── ipc_client.hpp
│   │   ├── src/
│   │   │   ├── protocol.cpp
│   │   │   └── ipc_client.cpp
│   │   └── tests/
│   ├── mumble/                      # Mumble Plugin API v1.0.x プラグイン
│   │   ├── CMakeLists.txt
│   │   ├── third_party/mumble/      # 公式 Mumble API v1.0.x ヘッダ & BSD-3-Clause LICENSE
│   │   ├── plugin.cpp
│   │   ├── mock_client.cpp          # E2E 用 kotoha_mock_client CLI
│   │   └── tests/                   # dlopen/LoadLibrary ABI 結合テスト
│   └── ts3/                         # (Phase 4) TeamSpeak 3 プラグイン
│
├── crates/                          # Rust コアクレート群 (Clean / Hexagonal Architecture)
│   ├── kotoha-protocol/             # IPC v1 コーデック & エンドポイント解決
│   ├── kotoha-core/                 # ドメイン・アプリケーション・Phase 1 インフラ/CLI
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── domain/              # 最内層: Entities/VOs, Ports (Traits), Pure Logic
│   │       ├── application/         # Use Cases: PipelineService, SourceSessionManager, TranslatorManager, GlossaryManager
│   │       ├── infrastructure/      # Adapters: IpcServerAdapter, AudioPreprocessor, TomlGlossaryRepository, MockAdapters
│   │       └── bin/
│   │           └── kotoha_ipc_monitor.rs
│   ├── kotoha-stt-sherpa/           # (Phase 2) sherpa-onnx ストリーミング STT アダプタ
│   ├── kotoha-translate-local/      # (Phase 2) ct2rs Local NMT アダプタ & ModelProfile カタログ/DL
│   ├── kotoha-translate-http/       # (Phase 2) OpenAI 互換 / DeepL / Google Cloud / GAS アダプタ
│   └── kotoha-e2e/                  # クロス言語 (C++ <-> Rust) IPC & 実音声パイプライン E2E スイート
│       ├── src/harness.rs
│       └── tests/
│           ├── ipc_single_source.rs
│           ├── ipc_multi_source.rs
│           └── real_audio_pipeline.rs
│
├── app/                             # デスクトップアプリ本体
│   ├── ui/                          # React + TypeScript + Vite + Vitest + Playwright
│   │   ├── package.json
│   │   ├── src/
│   │   ├── tests/
│   │   └── e2e/
│   └── src-tauri/                   # (Phase 3) Tauri v2 Rust バックエンド・設定・トレイ・バンドル
│       ├── Cargo.toml
│       ├── tauri.conf.json
│       └── src/
│
└── .github/
    └── workflows/
        ├── ci.yml                   # 3OS 静的解析・ビルド・単体テスト・UI/E2E & 実音声 E2E テスト
        └── release.yml              # 3OS リリースビルド・SHA-256・GitHub Releases 公開
```

---

## 6. テスト戦略・3OS CI/CD パイプライン (Testing & CI/CD Strategy)

### 6.1 3 層テスト戦略 & 単体テスト配置規約 (1 Source File : 1 Test File)

- **単体テスト配置規約（1 Source File : 1 Test File）**:
  - テストファイル肥大化を防ぎ対象モジュールとの対応関係を明確にするため、リポジトリ全体で **「テスト可能な 1 ソースファイル（モジュール）につき 1 テストファイル」** を原則とする（型定義のみ・エントリポイント・re-export 専用 `mod.rs`/`lib.rs` は除く）。
  - **Rust (`crates/*/tests/`)**: 各ソースモジュール `<module>.rs` に対応する統合テストファイル `crates/*/tests/<module>_test.rs`（共有モック・フィクスチャは `crates/*/tests/common/mod.rs`）を配置する（`cargo test` / `cargo nextest` が並列実行しやすい単位で分離）。
  - **C++ (`plugins/tests/`)**: 各実装モジュール `<module>.{hpp,cpp}` に対応する CTest ターゲット `plugins/tests/test_<module>.cpp` を配置する。
  - **UI (`app/ui/src/`)**: 各ソースファイル `<path>/<Module>.{ts,tsx}` 同一ディレクトリにコロケーションで `app/ui/src/<path>/<Module>.test.{ts,tsx}` を配置する（例: `src/data/modelCatalog.test.ts`, `src/components/SubtitleOverlay.test.tsx`, `src/components/TranslationSettings.test.tsx`, `src/App.test.tsx`）。

1. **Tier 1: コンポーネント単体テスト（言語別高速検証）**
   - **C++ (`CTest`, `plugins/tests/test_<module>.cpp`)**:
     - `protocol` の全 7 メッセージ往復、境界値（`payload_len == 65536`, `65537` 拒否、`NaN`/`Inf` 無害化、`channels` 境界）、リトルエンディアン検証。
     - `ring_buffer` の SPSC 並行プロデューサ/コンシューマ検証、満杯時ドロップ数および `seq` ギャップの一致検証。
   - **Rust (`cargo test --workspace`, `crates/*/tests/<module>_test.rs`)**:
     - `kotoha-protocol` の往復・不正フレーム（magic 不一致、version 不一致、64KB 超過、切り詰め、`flags` 予約ビット、NaN PCM、UTF-8 不正）検証、およびエンドポイント解決（`KOTOHA_IPC_ENDPOINT` 上書き・OS デフォルト）検証。
     - `kotoha-core` のモノラルダウンミックス、16 kHz リサンプリング、TOML 用語辞書読込・優先マージ、最長一致＆単語境界プレースホルダ保護/復元、Mock STT / Mock Translator によるパイプライン結合テスト。
   - **UI (`Vitest`, `app/ui/src/<path>/<Module>.test.{ts,tsx}`)**:
     - カタログ・FNV-1a 話者カラー・設定スキーマバリデーション（`modelCatalog.test.ts`）、字幕カード 3 段階状態遷移・行数制限・ロック状態（`SubtitleOverlay.test.tsx`）、プロバイダ/モデルプロファイル/ライセンス注意表示切替（`TranslationSettings.test.tsx`）、アプリ全体結合（`App.test.tsx`）。
2. **Tier 2: バイナリ ABI & 疎通結合テスト**
   - **C++ Mumble プラグイン ABI テスト (`plugins/tests/test_mumble_plugin_abi.cpp`)**: ビルドした共有ライブラリ（`.so`/`.dylib`/`.dll`）をテストランナーから動的ロード（`dlopen` / `LoadLibrary`）し、Mumble Plugin API v1.0.x 必須エクスポート関数群の解決、擬似コールバック（`isSpeech == true/false`、`mumble_onUserTalkingStateChanged`）の呼び出しと戻り値 `false`、メモリ破壊ゼロを検証。
   - **UI ブラウザ E2E (`Playwright`, `app/ui/e2e/`)**: モックイベントを注入した状態で字幕オーバーレイ描画・設定画面切替をヘッドレスブラウザで検証。
3. **Tier 3: クロス言語・実 OS IPC & 実音声パイプライン E2E テスト (`crates/kotoha-e2e`)**
   - 一時 `KOTOHA_IPC_ENDPOINT`（Unix ドメインソケットまたは固有 Named Pipe）を設定した上で `cargo nextest run --workspace`（または `cargo test -p kotoha-e2e`）により以下を自動検証する：
     1. **`ipc_single_source` / `ipc_multi_source`**: Rust 側 `kotoha-ipc-monitor` と C++ 側 `kotoha_mock_client` を実 OS IPC で接続し、`Hello` → `UserInfo` → `TalkingState` → 複数チャンネル/高レート合成 PCM `AudioFrame` → `Heartbeat` → `UserRemoved` → `SourceReset` の 1 バイト欠損もない受信と `SpeakerKey { source, user_id }` 分離を全 3 OS 上で検証。
     2. **`real_audio_pipeline`（`--features real-audio-e2e` + `KOTOHA_E2E_MODEL_DIR`）**: C++ 側 `kotoha_mock_client --wav` が実音声 WAV を OS IPC 経由でストリーミングし、`AudioPreprocessor` → ネイティブ `sherpa-onnx` Zipformer STT（TOML 用語辞書 Hotwords 含む）→ ローカル HTTP `OpenAiCompatibleTranslator` → 3 段階 `CollectingSubtitleSink` までをエンドツーエンドで検証。

### 6.2 3OS GitHub Actions CI & Release パイプライン

- **対象ランナーマトリクス**: `ubuntu-latest` (Linux x86_64), `windows-latest` (Windows x86_64), `macos-latest` (macOS arm64)。
- **`.github/workflows/ci.yml`**（PR / `main` push 時に実行）:
  1. **Lint / Format**:
     - Rust: `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`（および `real-audio-e2e` ジョブで `native-sherpa` / `real-audio-e2e` 機能付き Clippy）
     - C++: `clang-format --dry-run --Werror`, `clang-tidy`（Ubuntu で全 `plugins/common` / `plugins/mumble` ソース検証）
     - UI: `npm run lint`, `npm run typecheck`
  2. **Build & Test**:
     - C++: CMake Configure + Build + `ctest --output-on-failure`
     - Rust & Cross-Language E2E: `cargo build --release --bin kotoha-ipc-monitor` + `cargo nextest run --workspace`（全 3 OS で単体テストおよび `kotoha_mock_client` ↔ `kotoha-ipc-monitor` 実 IPC E2E を実行）
     - UI: `npm test` (Vitest) + Playwright E2E + `npm run build`
  3. **Real-Audio E2E (`real-audio-e2e` on `ubuntu-latest`)**:
     - キャッシュされた `sherpa-onnx` static lib（`SHERPA_ONNX_LIB_DIR`）および Zipformer 英語モデル（`KOTOHA_E2E_MODEL_DIR`）を用いて、`cargo nextest run -p kotoha-e2e --features real-audio-e2e --test real_audio_pipeline --no-capture` を PR 毎に実行。
- **`.github/workflows/release.yml`**（`v*` タグ push 時）:
  - 全 3 OS で CMake Release + CTest + `cargo nextest run --workspace` ゲートを通過後、プラグイン共有ライブラリおよびバイナリ（Phase 3 で Tauri `.msi` / `.dmg` / `.AppImage` / `.deb` を追加）をパッケージ。
  - 全配布成果物の `SHA256SUMS.txt` を生成して GitHub Releases へ自動公開。
  - Phase 4 で Windows/macOS コード署名（シークレット設定時のみ有効化するオプショナル構成）および Tauri Updater 用 `latest.json` 生成を追加。

---

## 7. 開発フェーズ計画 (Development Phases)

| Phase | 名称 | 主な成果物・マイルストーン | 関連 Issue |
| :--- | :--- | :--- | :--- |
| **Phase 1** | **Core IPC, Mumble Plugin, Repo & CI Foundation** | `DESIGN.md`・基盤設定ファイル、`kotoha-protocol`、`plugins/common`、`plugins/mumble` & `kotoha_mock_client`、`kotoha-core`（Clean/Hexagonal `domain`/`application`/`infrastructure`、`TomlGlossaryRepository` 基盤、Mock アダプタ、`kotoha-ipc-monitor`）、React+TS+Vite UI 骨格、クロス言語 E2E、3OS CI/Release | #1 〜 #7 |
| **Phase 2** | **Local STT & Pluggable Translation Engines** | `kotoha-stt-sherpa`（`sherpa-onnx` 話者別ストリーミング STT + `modified_beam_search` Hotwords / `hotwords_score`）、Pre-filter・Context/Glossary Manager（NMT/API 向け最長一致プレースホルダ保護 & LLM プロンプト注入）、`kotoha-translate-local`（`ct2rs` + CPU/GPU `ModelProfile` カタログ）& `kotoha-translate-http`（OpenAI 互換 LLM / DeepL / Google Cloud / GAS Webhook） | #8, #9 |
| **Phase 3** | **Tauri v2 Overlay UI, Tray & Auto-Setup Wizard** | Tauri v2 統合、透過クリックスルー字幕オーバーレイ、システムトレイ、初回セットアップウィザード（プラグイン自動配置・モデル DL/SHA-256 検証・Heartbeat 検知）、設定画面（`glossaries/*.toml` プロファイル管理 & `hotwords_score` 調整含む）、3OS バンドルリリース | #10 |
| **Phase 4** | **Multi-Source, Multilingual & Release Hardening** | `src_lang`/`tgt_lang` 多言語切替 UI & カタログ、`plugins/ts3`（TeamSpeak 3 プラグイン）＆ OS ループバック入力ソース、コード署名 & Tauri Updater | #11 |
