# Household Codex mode

Этот режим реализует правило «один дом — один постоянный чат». Все разрешённые
Яндекс-аккаунты и колонки дома продолжают один Codex thread и общую бытовую
историю. Разные дома получают разные thread, каталоги и permission profiles.
Homey gateways сохраняются как дополнительный, отдельно включаемый слой.

В обычной речи название дома не используется. Оно нужно администратору только
при создании дома и подтверждении первой привязки. После этого bridge сам
маршрутизирует запрос по паре `user_id` + `application_id`, которую присылает
Яндекс.

## 1. Конфигурация bridge

Скопируйте `config.example.toml` в `config.toml`. Для household mode обязательна
секция:

```toml
[runtime]
mode = "household_codex"
codex_socket = "/run/alice-codex/app-server.sock"
codex_cwd_root = "/srv/alice/houses"
permission_profile_prefix = "alice-house-"
chunk_limit = 850
homey_enabled = false
```

`homey_enabled = false` — безопасный разговорный режим первого этапа: bridge
не передаёт `mcp_servers` в Codex и не предлагает модели управление
устройствами. Когда gateway конкретного дома настроен и проверен, Homey можно
включить одним параметром `homey_enabled = true`; код и данные домов при этом
не меняются.

Сейчас этот флаг общий для одного процесса bridge. До ситуации, когда одному
дому уже нужен Homey, а другой должен оставаться chat-only, его нужно перенести
в настройки конкретного дома; текущий upgrade path — nullable boolean в
`houses` и чтение флага вместе с `homey_connector_id`.

Остальные legacy-секции пока остаются в файле для совместимости валидатора, но
ключ провайдера в household mode не читается. Процессу нужны:

- `WEBHOOK_SECRET` — секретная часть URL webhook;
- `DATABASE_URL` — строка подключения непривилегированной роли Postgres;
- `STATE_ENCRYPTION_KEY` — ровно 64 hex-символа, отдельный ключ для временных
  deferred/continuation payload и pairing HMAC;
- `CONFIG_PATH` — путь к `config.toml`, если он не стандартный;
- `RUST_LOG` — необязательный фильтр журналирования.

Секреты, идентификаторы Яндекса, коды привязки и текст разговоров не помещайте
в Git или обычные логи.

## 2. Изолированный Codex app-server

Используйте отдельного OS-пользователя и отдельный Codex home, не совпадающий с
операционным профилем Spain. Создайте пустой каталог каждого дома, например
`/srv/alice/houses/1`, и не монтируйте туда другие проекты или секреты.

Для каждого дома объявите read-only профиль. Минимальная основа для дома 1:

```toml
[permissions.alice-house-1]
extends = ":read-only"

[permissions.alice-house-1.network]
enabled = false
```

App-server запускается только на локальном Unix-сокете:

```bash
codex app-server --strict-config \
  --listen unix:///run/alice-codex/app-server.sock
```

Unix transport app-server является WebSocket-соединением поверх Unix-сокета;
bridge выполняет HTTP Upgrade сам. Не публикуйте этот сокет через TCP и выдайте
доступ к нему только группе bridge.

В базовой конфигурации Codex отключите shell, web search, apps и все Homey MCP.
Bridge повторяет запрет при каждом `thread/start` и `thread/resume`. Пока
`homey_enabled = false`, он не передаёт секцию `mcp_servers` вообще. После
явного включения он добавляет только gateway из поля `homey_connector_id`
текущего дома. Пример базовых ограничений:

```toml
[features]
shell_tool = false
unified_exec = false
skill_mcp_dependency_install = false

[tools]
web_search = false
view_image = false

[apps._default]
enabled = false

[mcp_servers.homey-mother]
command = "/usr/local/libexec/alice-homey-gateway"
args = ["--house", "mother"]
enabled = false
required = true
enabled_tools = [
  "list_attention_items",
  "get_device_state",
  "set_device_capability",
]
default_tools_approval_mode = "auto"
```

Gateway хранит Homey credential вне Codex и сам жёстко ограничивает дом,
устройства и возможности. Наличие system instructions не заменяет этот
allowlist.

## 3. Homey contract

Единственная разрешённая поверхность MCP:

- `list_attention_items()` — read-only; возвращает предупреждения по убыванию
  приоритета. В голосовой ответ попадает максимум первое;
- `get_device_state(device_id)` — read-only состояние разрешённого устройства;
- `set_device_capability(device_id, capability, value)` — меняет только
  allowlisted пару и делает обязательный read-back.

`set_device_capability` возвращает в `structuredContent` поля `requested`,
`observed` и boolean `verified`. Bridge не позволяет произнести успех при
`verified != true`. Разрешены только обратимые низкорисковые операции со
светом, кондиционером, климатом и мультимедиа. Замки, ворота, охрана, духовки,
нагреватели и неизвестные устройства gateway отклоняет до выполнения.

Для нескольких домов каждый gateway имеет отдельный идентификатор вида
`homey-...` и отдельный credential. Все gateways в базовом Codex config должны
быть `enabled = false`: bridge включает ровно один в thread-scoped override.

## 4. Создание дома и участников

Команды используют те же `DATABASE_URL` и `STATE_ENCRYPTION_KEY`, что bridge:

```bash
cargo run -p bridge-server --bin bridge-admin -- \
  house create --name "Дом мамы" --homey-connector homey-mother

cargo run -p bridge-server --bin bridge-admin -- \
  member add --house 1 --user-id '<YANDEX_USER_ID>' --role owner

cargo run -p bridge-server --bin bridge-admin -- \
  member add --house 1 --user-id '<MOTHER_YANDEX_USER_ID>' --role member
```

`user_id` берётся из тестового запроса Яндекс Диалогов или контролируемого
диагностического события, но не оставляется в обычном production-логе.

## 5. Одноразовая привязка колонки

1. Владелец делится приватной ссылкой навыка с нужным Яндекс-аккаунтом.
2. На новой колонке запускают навык. Backend узнаёт разрешённый аккаунт, но не
   знает эту поверхность, поэтому Алиса произносит нейтральный шестизначный код.
3. Владелец подтверждает дом:

```bash
cargo run -p bridge-server --bin bridge-admin -- \
  pairing approve --house 1 --code 123456
```

Код живёт 10 минут и хранится только как HMAC. Если один аккаунт состоит в
нескольких домах, именно `--house` выбирает дом для этой колонки. После
подтверждения привязка сохраняется в Postgres: каждый следующий запрос с этой
колонки автоматически попадает в нужный дом. Говорить название дома не нужно.

Новая колонка, сброс устройства или новый `application_id` требуют такой же
одноразовой привязки. Это осознанный потолок MVP; пересмотреть его можно, если
Яндекс добавит подписанный стабильный идентификатор физического Smart Home.

## 6. Read-only canary

До любого управления устройствами:

1. Сохраните вывод `codex --version`.
2. Выполните `codex app-server generate-json-schema --experimental --out DIR`
   и проверьте наличие `thread/start`, `thread/resume`, `turn/start`,
   `item/completed` и `turn/completed`.
3. Запустите один разговор без Homey и убедитесь, что ответ `thread/start`
   сообщает точный `activePermissionProfile = alice-house-N`, read-only sandbox
   и отсутствие network access. Bridge в противном случае закрывает запрос.
4. Пока `homey_enabled = false`, проверьте отсутствие `mcp_servers` в
   `thread/start` и `thread/resume`. После отдельного включения Homey выполните
   только `get_device_state` и `list_attention_items`; проверьте, что вызов
   другого gateway или неизвестного tool отклоняется.
5. Проверьте два аккаунта и две колонки одного дома: в таблице `houses` остаётся
   один `codex_thread_id`.

Первая live-запись в Homey выполняется только после отдельного подтверждения
владельца с названием конкретного устройства и обратимой команды. После неё
обязательно сверяется `observed` и `verified=true`.

## 7. Отзыв и rollback

Отключить участника или одну поверхность:

```bash
cargo run -p bridge-server --bin bridge-admin -- \
  member disable --house 1 --user-id '<YANDEX_USER_ID>'

cargo run -p bridge-server --bin bridge-admin -- \
  surface disable --application-id '<APPLICATION_ID>'
```

Для rollback остановите публичный webhook либо верните `runtime.mode =
"legacy"` и перезапустите только bridge. Не удаляйте `houses`, `surfaces` и
`codex_thread_id`: это сохраняет привязки и разговор для повторного включения.
Homey credential отзывается отдельно. Для полного отзыва доступа уберите также
приватный навык из соответствующего Яндекс-аккаунта.

## Известный эксплуатационный потолок

Локальная блокировка допускает только одну реплику на дом одновременно и
рассчитана на один экземпляр bridge. Перед `replica_count > 1` её надо заменить
Postgres advisory lock; измеримый триггер — запуск второй реплики, upgrade path
— один advisory lock на `house_id` вокруг всего Codex turn.

Полезные первичные ссылки: [Codex app-server](https://developers.openai.com/codex/app-server/),
[Codex configuration reference](https://developers.openai.com/codex/config-reference/),
[Codex MCP configuration](https://developers.openai.com/codex/mcp/).
