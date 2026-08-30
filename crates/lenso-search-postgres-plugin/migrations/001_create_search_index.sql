CREATE TABLE search_scopes (
    scope_kind text NOT NULL,
    scope_id text NOT NULL,
    index_revision bigint NOT NULL DEFAULT 0 CHECK (index_revision >= 0),
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (scope_kind, scope_id)
);

CREATE TABLE search_documents (
    scope_kind text NOT NULL,
    scope_id text NOT NULL,
    source_kind text NOT NULL,
    source_id text NOT NULL,
    search_text text NOT NULL,
    search_vector tsvector GENERATED ALWAYS AS (
        to_tsvector('simple'::regconfig, search_text)
    ) STORED,
    created_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT transaction_timestamp(),
    PRIMARY KEY (scope_kind, scope_id, source_kind, source_id),
    FOREIGN KEY (scope_kind, scope_id)
        REFERENCES search_scopes(scope_kind, scope_id)
);

CREATE INDEX search_documents_vector_idx ON search_documents USING gin(search_vector);
