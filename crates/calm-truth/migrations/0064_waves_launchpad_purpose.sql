ALTER TABLE waves ADD COLUMN purpose TEXT DEFAULT NULL;

CREATE UNIQUE INDEX idx_waves_one_launchpad
    ON waves(purpose) WHERE purpose = 'launchpad';
