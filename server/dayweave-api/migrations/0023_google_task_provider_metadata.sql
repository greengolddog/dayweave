-- Retain bounded, content-free Google Tasks semantics in the restricted sync
-- mapping without leaking provider details into canonical scheduling metadata.
ALTER TABLE provider_sync_mappings
    ADD COLUMN google_task_metadata jsonb;

ALTER TABLE provider_sync_mappings
    ADD CONSTRAINT provider_sync_mappings_google_task_metadata_shape_ck CHECK (
        google_task_metadata IS NULL
        OR (
            jsonb_typeof(google_task_metadata) = 'object'
            AND octet_length(google_task_metadata::text) <= 4096
            AND google_task_metadata ?& ARRAY[
                'hidden',
                'position',
                'completed',
                'completed_at',
                'title_truncated',
                'notes_truncated',
                'legacy_marker_stripped'
            ]
            AND google_task_metadata - ARRAY[
                'hidden',
                'position',
                'completed',
                'completed_at',
                'title_truncated',
                'notes_truncated',
                'legacy_marker_stripped'
            ] = '{}'::jsonb
            AND jsonb_typeof(google_task_metadata->'hidden') = 'boolean'
            AND (
                google_task_metadata->'position' = 'null'::jsonb
                OR (
                    jsonb_typeof(google_task_metadata->'position') = 'string'
                    AND length(google_task_metadata->>'position') BETWEEN 1 AND 1000
                )
            )
            AND jsonb_typeof(google_task_metadata->'completed') = 'boolean'
            AND (
                google_task_metadata->'completed_at' = 'null'::jsonb
                OR (
                    jsonb_typeof(google_task_metadata->'completed_at') = 'string'
                    AND length(google_task_metadata->>'completed_at') BETWEEN 1 AND 64
                )
            )
            AND jsonb_typeof(google_task_metadata->'title_truncated') = 'boolean'
            AND jsonb_typeof(google_task_metadata->'notes_truncated') = 'boolean'
            AND jsonb_typeof(google_task_metadata->'legacy_marker_stripped') = 'boolean'
        )
    );
