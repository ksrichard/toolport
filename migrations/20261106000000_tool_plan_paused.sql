-- The plan pause of a tool is stored now. Before, the server computed it from the tool order,
-- so a tool that the user turned off gave its place to the next paused tool.
-- A paused tool is off. An upgrade turns the paused tools on again, up to the new limit.
ALTER TABLE tools
    ADD COLUMN plan_paused BOOLEAN NOT NULL DEFAULT false,
    ADD CONSTRAINT tools_paused_is_off CHECK (NOT (enabled AND plan_paused));

-- The tool limit that the last reconcile applied. NULL means no limit. When the limit of the
-- plan differs from this value, `plan::reconcile_tools` pauses or restores tools.
ALTER TABLE users ADD COLUMN tool_limit_applied INTEGER;

-- Apply the current limits once: on each server, the enabled tools after the first N by id
-- become paused.
WITH ranked AS (
    SELECT t.id, p.max_tools_per_server AS max,
           row_number() OVER (PARTITION BY t.server_id ORDER BY t.id) AS n
    FROM tools t
    JOIN servers s ON s.id = t.server_id
    JOIN users u ON u.id = s.user_id
    JOIN plans p ON p.id = u.plan_id
    WHERE t.enabled
)
UPDATE tools SET enabled = false, plan_paused = true
FROM ranked
WHERE tools.id = ranked.id AND ranked.max IS NOT NULL AND ranked.n > ranked.max;

UPDATE users u SET tool_limit_applied = p.max_tools_per_server
FROM plans p WHERE p.id = u.plan_id;

-- New users start with the limit of the default plan.
CREATE OR REPLACE FUNCTION users_default_plan() RETURNS trigger AS $$
BEGIN
    IF NEW.plan_id IS NULL THEN
        NEW.plan_id := (SELECT id FROM plans WHERE is_default);
    END IF;
    NEW.tool_limit_applied := (SELECT max_tools_per_server FROM plans WHERE id = NEW.plan_id);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;
