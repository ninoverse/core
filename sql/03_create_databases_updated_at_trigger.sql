CREATE TRIGGER trg_databases_updated_at BEFORE
UPDATE ON "databases" FOR EACH ROW EXECUTE FUNCTION set_updated_at ();
