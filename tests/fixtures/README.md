# TLSテスト用データ

`localhost.pem` と `localhost.p12` はlocalhost専用の自己署名証明書と秘密鍵です。公開してよい試験専用データで、実サーバーの資格情報ではありません。PKCS#12のパスフレーズは `local-test-only` です。

テストはこの証明書をそのテストのTLSコネクターにだけ追加します。OSの信頼ストアは変更せず、製品の接続コードで検証を無効にしません。

有効期限は生成から365日です。macOSが長期間のサーバー証明書を拒否するため、テストデータを更新する場合も365日以内にします。次のコマンドをリポジトリルートで実行できます（OpenSSL 3）。

```sh
openssl req -x509 -newkey rsa:2048 -nodes \
  -keyout /tmp/rushvn-test-key.pem \
  -out tests/fixtures/localhost.pem -days 365 \
  -subj '/CN=localhost' \
  -addext 'subjectAltName=DNS:localhost' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'extendedKeyUsage=serverAuth'
openssl pkcs12 -export -legacy \
  -inkey /tmp/rushvn-test-key.pem -in tests/fixtures/localhost.pem \
  -out tests/fixtures/localhost.p12 -passout pass:local-test-only
```

秘密鍵を含む試験データを製品の接続設定や配布アセットに転用しないでください。
