CREATE OR REPLACE FUNCTION set_updated_at()
RETURNS TRIGGER AS $$
BEGIN
  IF NEW IS DISTINCT FROM OLD THEN
    NEW.updated_at = CURRENT_TIMESTAMP;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TABLE
  IF NOT EXISTS "databases" (
    "id" SERIAL PRIMARY KEY,
    "name" VARCHAR(255) NOT NULL,
    "description" TEXT,
    "status" VARCHAR(255) NOT NULL,
    "technology" VARCHAR(255) NOT NULL,
    "created_at" TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    "updated_at" TIMESTAMP DEFAULT CURRENT_TIMESTAMP
  );

CREATE TRIGGER trg_databases_updated_at BEFORE
UPDATE ON "databases" FOR EACH ROW EXECUTE FUNCTION set_updated_at ();

INSERT INTO
  "databases" (name, description, status, technology)
VALUES
  (
    'MAIN_DB',
    'The core db used by this application, keep tracks of created databases',
    'CREATED',
    'postgres'
  );