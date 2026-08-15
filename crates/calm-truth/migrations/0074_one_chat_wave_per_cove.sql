CREATE UNIQUE INDEX idx_waves_one_chat_per_cove
ON waves(cove_id)
WHERE purpose = 'cove-chat';
