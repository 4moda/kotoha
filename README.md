# Kotoha

**Kotoha** は、ボイスチャット（Mumble 対応、将来的に TeamSpeak 3・OS ループバックへ拡張予定）の音声をリアルタイムに文字起こし・翻訳し、透過クリックスルー字幕オーバーレイとして表示するマルチプラットフォーム（Windows / macOS / Linux）対応デスクトップアプリケーションです。

詳細なアーキテクチャおよびバイナリプロトコル仕様は [DESIGN.md](./DESIGN.md) を参照してください。

---

## 主な特徴

- **Zero-Config Default**: 初回起動ウィザードによるプラグイン自動配置・ローカルモデル自動ダウンロード（SHA-256 検証付き）で、API キー不要・完全オフラインからすぐに利用可能。
- **Subtitle-Only（字幕特化）**: 音声合成（TTS）を持たない軽量・低遅延な透過クリックスルーオーバーレイ（3 段階ライブ更新、話者カラー自動割当）。
- **Thin Plugin / Thick App**: ボイスチャット側の C++20 プラグインはロックフリー SPSC リングバッファ＋OS ネイティブ IPC 送信のみを担当し、音声処理・STT・翻訳・UI はすべて独立した Rust / Tauri v2 プロセス（Clean / Hexagonal Architecture）で安全に実行。
- **環境別モデル選択 & 差し替え可能な翻訳プロバイダ**:
  1. **Local NMT (`ct2rs` / CTranslate2)** — CPU 軽量（FuguMT / OPUS-MT）から GPU 多言語（NLLB-200）までハードウェア・言語ペアに応じた `ModelProfile` 単位で選択可能（完全無料・オフライン・低遅延）
  2. **OpenAI 互換 LLM API** — OpenAI / Gemini / ローカル Ollama・LM Studio（GPU / Apple Silicon 対応、会話履歴 5〜10 件＋用語辞書）
  3. **専用翻訳 API / ユーザー管理プロキシ** — DeepL（Free / Pro）、Google Cloud Translation API（公式 v2 / v3）、ユーザー独自 GAS (`LanguageApp.translate()`) Webhook（非公式スクレイピングは非搭載）
- **汎用カスタム用語辞書 (`glossaries/*.toml`)**: 単一の TOML 用語マップを **STT Hotwords (Context Biasing)** と **翻訳固定訳語（プレースホルダ保護 / プロンプト注入）** で共通利用可能（ドメイン非依存）。

---

## アーキテクチャ概要

```text
[Mumble / TS3 (C++20 Thin Plugin)]
   │  Audio Callback -> Lock-Free SPSC Ring Buffer -> IPC Worker (1s Heartbeat / Reconnect)
   ▼  OS Native IPC (Windows Named Pipe / POSIX Unix Domain Socket, Kotoha IPC v1 LE)
[Kotoha Core (Rust + tokio, Clean / Hexagonal Architecture)]
   │  Infra (IpcServerAdapter, Resampler) -> Application (PipelineService) -> Domain (Ports)
   │  SttEngine (sherpa-onnx) -> Translator (Local NMT ModelProfile / LLM / DeepL / Google / GAS)
   ▼  Tauri v2 Event Bridge
[Overlay & Settings UI (React + TypeScript + Vite)]
```

---

## リポジトリ構成

- [`DESIGN.md`](./DESIGN.md) — 全体設計書・IPC プロトコル v1 バイナリ仕様（Single Source of Truth）
- `plugins/` — C++20 プラグイン群（`common`, `mumble`, 将来 `ts3`）
- `crates/` — Rust ワークスペース（`kotoha-protocol`, `kotoha-core` [`domain`/`application`/`infrastructure`], `kotoha-stt-sherpa`, `kotoha-translate-local`, `kotoha-translate-http`, `kotoha-e2e` [クロス言語 C++ ↔ Rust IPC & 実音声 E2E スイート]）
- `app/ui/` — React + TypeScript + Vite フロントエンド
- `app/src-tauri/` — Tauri v2 デスクトップ統合
- `.github/workflows/` — Windows / macOS / Linux 3OS CI & リリースワークフロー

---

## ビルド・テストコマンド

> [!NOTE]
> 各コンポーネントは開発フェーズ（Phase 1〜4）に沿って順次実装されます。詳細は [DESIGN.md](./DESIGN.md) および各 Issue を参照してください。

### C++ プラグイン (`plugins/`)

```bash
cmake -S plugins -B plugins/build -DCMAKE_BUILD_TYPE=Release
cmake --build plugins/build --config Release
ctest --test-dir plugins/build --output-on-failure -C Release

# フォーマット検証
find plugins \( -path 'plugins/build' -o -path 'plugins/mumble/third_party' \) -prune -o -type f \( -name '*.h' -o -name '*.hpp' -o -name '*.cpp' \) -print0 | xargs -0 -r clang-format --dry-run --Werror
```

### Rust コア & クロス言語 E2E (`crates/`)

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings

# Rust 単体テスト & クロス言語 IPC E2E (`ipc_single_source`, `ipc_multi_source`)
# ※ クロス言語 E2E を実際に実行するには、先に C++ テストバイナリ (`plugins/build/tests/kotoha_mock_client`) と
#   `cargo build --release --bin kotoha-ipc-monitor` をビルドしてください:
cmake -S plugins -B plugins/build && cmake --build plugins/build
cargo build --release --bin kotoha-ipc-monitor

# ローカル環境で `kotoha_mock_client` が未ビルドの場合、IPC E2E テストは案内メッセージを出力してスキップします。
# 未ビルド時にスキップせず失敗させたい場合（CI と同等の厳格チェック）は `KOTOHA_REQUIRE_CPP_E2E=1` を指定してください:
KOTOHA_REQUIRE_CPP_E2E=1 cargo nextest run --workspace
# （または KOTOHA_REQUIRE_CPP_E2E=1 cargo test --workspace）

# Doctest の実行（cargo-nextest は doctest を実行しないため個別実行）
cargo test --workspace --doc

# 実音声 E2E (`C++ kotoha_mock_client --wav` -> OS IPC -> sherpa-onnx Zipformer STT -> HTTP 翻訳)
KOTOHA_E2E_MODEL_DIR=/path/to/sherpa-onnx-streaming-zipformer-en-20M-2023-02-17-mobile \
  SHERPA_ONNX_LIB_DIR=/path/to/sherpa-onnx-v2.0.3-linux-x64-static-lib/lib \
  cargo test -p kotoha-e2e --features real-audio-e2e --test real_audio_pipeline -- --nocapture
# （cargo-nextest 使用時: cargo nextest run -p kotoha-e2e --features real-audio-e2e --test real_audio_pipeline --no-capture）
```

### UI フロントエンド (`app/ui/`)

```bash
cd app/ui
npm ci
npm run lint
npm run typecheck
npm test
npm run build
npx playwright test
```

---

## ライセンスと帰属表示

- **Kotoha 本体コード**: [MIT License](./LICENSE) (`Copyright (c) 2026 4moda`)
- **同梱ヘッダ・外部モデル**:
  - Mumble Plugin API v1.0.x ヘッダ (`plugins/mumble/third_party/mumble/`) は **BSD-3-Clause** ライセンスに従います。
  - ローカル STT / NMT モデルは本体バイナリには同梱せず、初回セットアップ時にダウンロードします。モデルごとに固有のライセンス（例: FuguMT `CC-BY-SA-4.0`、OPUS-MT `CC-BY-4.0`、NLLB-200 `CC-BY-NC-4.0` 非商用限定）が適用されます。詳細は [DESIGN.md](./DESIGN.md) を参照してください。
