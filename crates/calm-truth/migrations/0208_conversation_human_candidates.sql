-- Title lookup follows declared Human presentation order without scanning imported bodies.
CREATE INDEX conversation_presentations_human_order
ON conversation_presentations(card_id, sequence)
WHERE source_kind = 'human_message';
