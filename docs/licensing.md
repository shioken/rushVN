# ライセンスと配布

## 公開方針

ソースコードのみを公開し、利用者が手元でビルドする。実行ファイル・インストーラーの配布は予定していない。OS別の配布パッケージ、署名・公証、同梱ネイティブライブラリの配布確認は、今回の公開準備の対象外とする。

## rustVNの独自部分

本体の独自コード、スクリプト、ドキュメント、架空のデモデータ、生成したTLS試験用素材、スクリーンショットの独自部分には、ルートの [MIT License](../LICENSE) を適用する。著作権表示は `Copyright (c) 2026 rustVN Contributors` とし、個人の氏名・メールアドレスは追加しない。

第三者のライブラリ、フォント、ライセンス文書などをMITへ再ライセンスするものではない。スクリーンショットにも第三者由来の表示要素があり、その権利まで独占・移転するものではない。実際に受信したニュース記事や利用者のフォントは、この許諾の対象外。

WinVNはUIの参考対象として言及しており、WinVNのソース、ロゴ、マニュアル本文を同梱していない。要件書の参考資料はリンクであり、rustVNのMITの適用対象ではない。

## 第三者のコードとフォント

[THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) に、Cargo.lockの依存パッケージ、版、宣言されたライセンス、原文の著作権表示・ライセンス本文、ソース入手先を記録する。全OS・オプション・開発用を含む一覧であり、各実行ファイルにすべてが組み込まれるわけではない。

MITを選択できる依存はMITの選択肢を使用する。`AND`で指定された追加条件は保持する。例えばencoding_rsの変換表にはBSD-3-Clause、unicode-identにはUnicodeの条件もある。上流に含まれる選択肢の本文は、採用しないものも証跡として残す。

`directories → dirs-sys → option-ext 0.2.0` のoption-extはMPL-2.0で、無改変で使用する。対応ソースとライセンスは [0.2.0の配布アーカイブ](https://crates.io/api/v1/crates/option-ext/0.2.0/download) から取得できる。実行ファイルの配布先にもこの入手方法を伝える。改変した場合は、その改変を含む対応ソースをMPL-2.0で提供する。rustVNの独立したファイルまでMPLへ変更する必要はない。[Mozillaの説明](https://www.mozilla.org/en-US/MPL/2.0/FAQ/)

日本語フォントは利用者のOSまたは `RUSTVN_FONT` から読み込む。フォントファイルの再配布は行わない。将来同梱する場合は、そのフォントの再配布条件を別途確認する。

eguiの標準フォントは実行ファイルに組み込まれるため、次の本文・表示を第三者ライセンス一覧に含める。

| フォント | 条件 |
| --- | --- |
| Hack | MIT、Bitstream Vera由来の条件・表示、DejaVuの表示 |
| Noto Emoji | SIL Open Font License 1.1 |
| Ubuntu Light | Ubuntu Font Licence 1.0 |
| emoji-icon-font | MIT |

通常のSQLiteはpublic domainで、Rustラッパーには別途ライセンスがある。一覧にはcrateに入っている別構成向けの文書も含まれるが、SQLCipherを使用しているという意味ではない。

## 更新と検証

```sh
cargo fetch --locked
python3 scripts/generate_licenses.py
python3 scripts/generate_licenses.py --check
python3 -m unittest discover -s tests -p 'test_*.py'
```

生成処理はネットワークを使わず、ローカルに取得済みのcrateと [補完資料](../licenses/upstream.json) を使う。補完資料にはcrateに欠けていたライセンス本文を保存し、対応する公開元コミットのURLと本文のSHA-256を記録する。標準文だけを補ったdispatch・enum-map・hexf-parseについては標準文の出典を示し、著作者表示はcrateのメタデータ・ソースから収集する。本文を独自に書き換えない。

依存更新では、ライセンス表記、同梱データ・フォント、ソース内の追加条件、不足する原文、対応ソースへのリンクをレビューする。生成スクリプトはファイル名と著作権ヘッダーによる収集なので、任意のファイルに埋め込まれた条件の完全な自動判定ではない。

原文と出典を確認した後だけ、`scripts/license-notices.json` のSHA-256を更新する。ここで許可するのは、固定された2ファイル内の第三者のメールアドレス表示だけ。個人用パス、秘密鍵、トークンなどの検査は継続する。利用者の個人情報を入れるための例外ではない。

```sh
python3 scripts/check_privacy.py --worktree
python3 scripts/check_privacy.py
python3 scripts/check_privacy.py --history
```

`--worktree` は作業中のファイル、引数なしはステージ済みのファイル、`--history` は到達可能なコミットを検査する。新しい通知文はハッシュの更新も一緒にステージする。

## 参考：第三者が実行ファイルを再配布する場合

配布ZIP等に、実行ファイルとともに `LICENSE`、`THIRD_PARTY_NOTICES.md`、`docs/licensing.md` を同じ相対配置で含める。アプリバンドルに格納する場合も利用者が読める場所に置き、README等から案内する。ソース取得URLが有効であることも確認する。

この一覧はCargo依存の資料であり、Rust標準ライブラリや配布物に追加するネイティブライブラリの条件は、実際の配布物と使用ツールチェーンに合わせて確認・添付する。特にLinuxのOpenSSL・D-Bus、各OSの追加DLL等は実際に同梱する版とリンク方式を確認する。OSに存在するライブラリの利用と、そのライブラリ自体の再配布は区別する。

上記は再配布者向けの参考事項であり、本プロジェクトのソース公開に向けた未完了作業ではない。
