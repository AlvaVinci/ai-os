# AI OS

個人・開発者向けの、ローカル優先なAIネイティブOS実行基盤です。

> [!IMPORTANT]
> 現在は構想・設計段階です。起動可能なOSや実用ランタイムはまだ提供していません。

## ビジョン

従来のOSがプロセス、ファイル、ウィンドウを中心に設計されているのに対し、AI OSは次の要素を第一級の概念として扱います。

- **Task**: 達成する目的、制約、期限、予算
- **Agent**: タスクを計画・実行する主体
- **Model**: ローカルまたは外部の推論資源
- **Context**: 出所と有効期限を持つ短期・長期の文脈
- **Capability**: ファイル、ネットワーク、ツールへの明示的な権限
- **Budget**: CPU、GPU、RAM、VRAM、電力、時間、外部API費用の上限
- **Event**: 判断と操作を再現するための監査記録

AIによる判断は、決定論的なポリシー・権限検証を通して実行します。モデル出力を、そのまま特権操作として扱いません。

## 初期スコープ

最初の成果物は、Linux上で動作するユーザー空間ランタイムです。

- 構造化されたタスクの受付と状態管理
- エージェントの起動、監視、停止、再試行
- 能力ベースの権限管理と人間による承認
- ローカルモデルを優先するモデルルーティング
- CPU、GPU、メモリ、時間を考慮した資源管理
- 追記型イベントログによる監査と再現
- CLIおよび安定したローカルAPI

独自カーネルは初期スコープに含めません。ユーザー空間で計測し、Linuxでは解決できない要件が明確になった時点で、カーネル拡張または独自カーネルを評価します。

## 設計原則

1. **Local first** — データと推論は、明示的に許可されない限り端末外へ送信しない。
2. **Deterministic enforcement** — AIは方針を提案し、決定論的な実行層が権限と制約を強制する。
3. **Least privilege** — エージェントにはタスクに必要な最小権限だけを付与する。
4. **Observable and replayable** — 重要な判断、承認、操作、資源消費を追跡可能にする。
5. **Model agnostic** — 特定のモデル、ベンダー、アクセラレータに中核設計を依存させない。
6. **Compatibility first** — Linuxのプロセス、ファイル、コンテナ、既存開発ツールを活用する。

## アーキテクチャ

```text
CLI / Local API / Future GUI
            |
Task & Agent Supervisor
            |
Policy / Capability / Approval
            |
Model Router & Resource Scheduler
            |
Model Runtime / Context Store / Event Log
            |
Linux Kernel / Containers / Hardware
```

詳しくは以下を参照してください。

- [ビジョン](docs/vision.md)
- [アーキテクチャ](docs/architecture.md)
- [MVP仕様](docs/mvp-spec.md)
- [ロードマップ](docs/roadmap.md)

## プロジェクト状況

現在のフェーズは **Phase 0: Foundation** です。用語、脅威境界、MVP受け入れ条件を固めています。

設計上の論点やユースケースは、GitHub Issuesで提案してください。実装開始前でも、具体的な制約や失敗例を伴う議論を歓迎します。

## コントリビューション

[CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。セキュリティ上の問題は公開Issueに投稿せず、[SECURITY.md](SECURITY.md) の手順に従ってください。

## ライセンス

Apache License 2.0です。詳細は [LICENSE](LICENSE) を参照してください。
