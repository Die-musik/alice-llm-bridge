CREATE TABLE houses (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    codex_thread_id TEXT,
    homey_connector_id TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE house_members (
    house_id BIGINT NOT NULL REFERENCES houses(id) ON DELETE CASCADE,
    yandex_user_id TEXT NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('owner', 'member')),
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (house_id, yandex_user_id)
);

CREATE TABLE surfaces (
    application_id TEXT PRIMARY KEY,
    house_id BIGINT NOT NULL,
    yandex_user_id TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (house_id, application_id),
    FOREIGN KEY (house_id, yandex_user_id)
        REFERENCES house_members(house_id, yandex_user_id) ON DELETE CASCADE
);

CREATE TABLE pairing_requests (
    id BIGSERIAL PRIMARY KEY,
    house_id BIGINT,
    yandex_user_id TEXT NOT NULL,
    application_id TEXT NOT NULL UNIQUE,
    code_hash BYTEA NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    approved_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    FOREIGN KEY (house_id, yandex_user_id)
        REFERENCES house_members(house_id, yandex_user_id) ON DELETE CASCADE
);

CREATE INDEX pairing_requests_expiry_idx ON pairing_requests (expires_at);

CREATE TABLE pending_replies (
    house_id BIGINT NOT NULL,
    application_id TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('thinking', 'ready')),
    nonce BYTEA,
    ciphertext BYTEA,
    key_version SMALLINT,
    expires_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (house_id, application_id),
    FOREIGN KEY (house_id, application_id)
        REFERENCES surfaces(house_id, application_id) ON DELETE CASCADE,
    CHECK (
        (status = 'thinking' AND nonce IS NULL AND ciphertext IS NULL AND key_version IS NULL)
        OR
        (status = 'ready' AND nonce IS NOT NULL AND ciphertext IS NOT NULL AND key_version IS NOT NULL)
    )
);

CREATE INDEX pending_replies_expiry_idx ON pending_replies (expires_at);

CREATE TABLE continuations (
    house_id BIGINT NOT NULL,
    application_id TEXT NOT NULL,
    nonce BYTEA NOT NULL,
    ciphertext BYTEA NOT NULL,
    key_version SMALLINT NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (house_id, application_id),
    FOREIGN KEY (house_id, application_id)
        REFERENCES surfaces(house_id, application_id) ON DELETE CASCADE
);

CREATE INDEX continuations_expiry_idx ON continuations (expires_at);
