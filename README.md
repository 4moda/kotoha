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
- `crates/` — Rust ワークスペース（`kotoha-protocol`, `kotoha-core` [`domain`/`application`/`infrastructure`], 将来 `kotoha-stt-sherpa` / `kotoha-translate-local` / `kotoha-translate-http`）
- `app/ui/` — React + TypeScript + Vite フロントエンド
- `app/src-tauri/` — Tauri v2 デスクトップ統合
- `tests/e2e/` — クロス言語（C++ プラグイン ↔ Rust IPC サーバー）E2E テストスイート
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

### Rust コア (`crates/`)

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

### UI フロントエンド (`app/ui/`)

```bash
cd app/ui
npm ci
npm run lint
npm run typecheck
npm test
npx playwright test
npm run build
```

---

## ライセンスと帰属表示

- **Kotoha 本体コード**: [MIT License](./LICENSE) (`Copyright (c) 2026 4moda`)
- **同梱ヘッダ・外部モデル**:
  - Mumble Plugin API v1.0.x ヘッダ (`plugins/mumble/third_party/mumble/`) は **BSD-3-Clause** ライセンスに従います。
  - ローカル STT / NMT モデルは本体バイナリには同梱せず、初回セットアップ時にダウンロードします。モデルごとに固有のライセンス（例: FuguMT `CC-BY-SA-4.0`、OPUS-MT `CC-BY-4.0`、NLLB-200 `CC-BY-NC-4.0` 非商用限定）が適用されます。詳細は [DESIGN.md](./DESIGN.md) を参照してください。
