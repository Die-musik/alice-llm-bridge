# Registering the private Alice skill

These steps connect a running `bridge-server` to a private skill in Yandex
Dialogs. Console labels can change; use the equivalent current field if the UI
wording differs.

1. Open <https://dialogs.yandex.ru/developer> and create a skill.

2. Fill in the skill settings:

   - **Название навыка** — try `ChatGPT`. If Yandex rejects the activation name,
     use `Домашний GPT`; only the spoken launch phrase changes.
   - **Активационные имена** — add alternatives only if recognition needs them.
   - **Описание** — for example: “Личный голосовой помощник семьи с общей
     историей разговора и управлением разрешёнными устройствами дома.”
   - **Голос** — choose the Yandex voice you want to hear. The backend returns
     text; speech is synthesized by Alice, not by ChatGPT voice mode.
   - **Backend / Webhook URL**:

     ```text
     https://<your-domain>/alice/webhook/<WEBHOOK_SECRET>
     ```

   - **Хранилище** — not required; persistent household state lives in Postgres
     and the conversation itself lives in the Codex thread.
   - **Тип доступа** — `Приватный`.
   - **Доступные поверхности** — at least `Яндекс Станция`; a screen is not
     required.
   - Complete the remaining required category, developer and icon fields.

3. Save the skill and use the console test chat to verify that the secret URL
   reaches the webhook. Then keep `Тип доступа = Приватный`, click
   **Опубликовать** and wait for private publication. Yandex requires this before
   voice testing on a phone or Station; it does not put the skill in the public
   catalog. The console then provides a one-time access link for invited users.

4. Add the owner's Yandex `user_id` to the household with `bridge-admin member
   add`. Do not rely only on draft/private visibility as authorization.

5. Open the skill on the first Station. An unbound but approved account hears a
   six-digit code. Confirm it once with:

   ```bash
   cargo run -p bridge-server --bin bridge-admin -- \
     pairing approve --house 1 --code 123456
   ```

   From then on, that Station is routed to the house automatically. You say only
   “Алиса, запусти навык ChatGPT” and then speak normally; you do not name the
   house on each request.

6. To give access to another Yandex account, share the private-skill link, add
   that account as a `member`, launch the skill on its Station and approve its
   one-time code for the same house. Both accounts then share the same household
   Codex thread.

7. For a second physical home, create a second house and approve that home's
   Stations with `pairing approve --house <SECOND_ID>`. Even if the same Yandex
   account belongs to both homes, each paired `application_id` keeps its own
   fixed home route.

Detailed server, Codex and Homey setup is in
[`household-setup.md`](household-setup.md).
