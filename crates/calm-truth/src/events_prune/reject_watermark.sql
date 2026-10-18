CREATE TRIGGER reject_test_watermark BEFORE INSERT ON retention_meta
BEGIN
    SELECT RAISE(ABORT, 'test watermark rejected');
END
