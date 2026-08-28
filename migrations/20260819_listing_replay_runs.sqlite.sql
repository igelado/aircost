-- Add resumable coordination for trusted-capture listing replay.

BEGIN IMMEDIATE;

-- A marker-present rerun must prove the exact canonical replay contract before
-- any replay DDL is attempted. The duplicate sentinel rows deliberately violate
-- the migration-name primary key when neither a pristine install nor an exact
-- installed contract is present; the statement and enclosing transaction then
-- leave the hostile schema untouched.
WITH expected_replay_guard_definitions(name, definition) AS (
  VALUES
    ('listing_replay_run_items_checkpoint_exact_insert',
     CAST(X'4352454154452054524947474552206c697374696e675f7265706c61795f72756e5f6974656d735f636865636b706f696e745f65786163745f696e736572740a4245464f524520494e53455254204f4e' AS TEXT) ||
     CAST(X'206c697374696e675f7265706c61795f72756e5f6974656d730a5748454e204e45572e65787472616374696f6e5f7374617465203d20277375636365656465642720414e44204e4f5420455849535453' AS TEXT) ||
     CAST(X'20280a202053454c45435420310a202046524f4d20706c7567696e5f7375626d697373696f6e73207375626d697373696f6e0a20204a4f494e20706c7567696e5f696e7374616c6c7320696e7374616c' AS TEXT) ||
     CAST(X'6c204f4e20696e7374616c6c2e6964203d207375626d697373696f6e2e706c7567696e5f696e7374616c6c5f69640a20205748455245207375626d697373696f6e2e6964203d204e45572e706c756769' AS TEXT) ||
     CAST(X'6e5f7375626d697373696f6e5f69640a20202020414e44207375626d697373696f6e2e72656e64657265645f68746d6c5f736861323536203d204e45572e65787065637465645f72656e64657265645f' AS TEXT) ||
     CAST(X'68746d6c5f7368613235360a20202020414e44207375626d697373696f6e2e6578747261637465645f6c697374696e675f6a736f6e204953204e45572e6578747261637465645f6c697374696e675f6a' AS TEXT) ||
     CAST(X'736f6e0a20202020414e44207375626d697373696f6e2e65787472616374696f6e5f6572726f72204953204e554c4c0a20202020414e44206a756c69616e646179287375626d697373696f6e2e737562' AS TEXT) ||
     CAST(X'6d69747465645f617429204953204e4f54204e554c4c0a20202020414e4420280a202020202020696e7374616c6c2e7265766f6b65645f6174204953204e554c4c0a2020202020204f5220280a202020' AS TEXT) ||
     CAST(X'20202020206a756c69616e64617928696e7374616c6c2e7265766f6b65645f617429204953204e4f54204e554c4c0a2020202020202020414e44206a756c69616e646179287375626d697373696f6e2e' AS TEXT) ||
     CAST(X'7375626d69747465645f617429203c3d206a756c69616e64617928696e7374616c6c2e7265766f6b65645f6174290a202020202020290a20202020290a290a424547494e0a202053454c454354205241' AS TEXT) ||
     CAST(X'4953452841424f52542c20277265706c61792065787472616374696f6e207472616e736974696f6e20646f6573206e6f74206d617463682069747320657861637420636865636b706f696e7427293b0a' AS TEXT) ||
     CAST(X'454e44' AS TEXT)),
    ('listing_replay_run_items_checkpoint_exact_update',
     CAST(X'4352454154452054524947474552206c697374696e675f7265706c61795f72756e5f6974656d735f636865636b706f696e745f65786163745f7570646174650a4245464f524520555044415445204f4e' AS TEXT) ||
     CAST(X'206c697374696e675f7265706c61795f72756e5f6974656d730a5748454e204e45572e65787472616374696f6e5f7374617465203d20277375636365656465642720414e44204e4f5420455849535453' AS TEXT) ||
     CAST(X'20280a202053454c45435420310a202046524f4d20706c7567696e5f7375626d697373696f6e73207375626d697373696f6e0a20204a4f494e20706c7567696e5f696e7374616c6c7320696e7374616c' AS TEXT) ||
     CAST(X'6c204f4e20696e7374616c6c2e6964203d207375626d697373696f6e2e706c7567696e5f696e7374616c6c5f69640a20205748455245207375626d697373696f6e2e6964203d204e45572e706c756769' AS TEXT) ||
     CAST(X'6e5f7375626d697373696f6e5f69640a20202020414e44207375626d697373696f6e2e72656e64657265645f68746d6c5f736861323536203d204e45572e65787065637465645f72656e64657265645f' AS TEXT) ||
     CAST(X'68746d6c5f7368613235360a20202020414e44207375626d697373696f6e2e6578747261637465645f6c697374696e675f6a736f6e204953204e45572e6578747261637465645f6c697374696e675f6a' AS TEXT) ||
     CAST(X'736f6e0a20202020414e44207375626d697373696f6e2e65787472616374696f6e5f6572726f72204953204e554c4c0a20202020414e44206a756c69616e646179287375626d697373696f6e2e737562' AS TEXT) ||
     CAST(X'6d69747465645f617429204953204e4f54204e554c4c0a20202020414e4420280a202020202020696e7374616c6c2e7265766f6b65645f6174204953204e554c4c0a2020202020204f5220280a202020' AS TEXT) ||
     CAST(X'20202020206a756c69616e64617928696e7374616c6c2e7265766f6b65645f617429204953204e4f54204e554c4c0a2020202020202020414e44206a756c69616e646179287375626d697373696f6e2e' AS TEXT) ||
     CAST(X'7375626d69747465645f617429203c3d206a756c69616e64617928696e7374616c6c2e7265766f6b65645f6174290a202020202020290a20202020290a290a424547494e0a202053454c454354205241' AS TEXT) ||
     CAST(X'4953452841424f52542c20277265706c61792065787472616374696f6e207472616e736974696f6e20646f6573206e6f74206d617463682069747320657861637420636865636b706f696e7427293b0a' AS TEXT) ||
     CAST(X'454e44' AS TEXT)),
    ('listing_replay_run_items_completed_immutable_update',
     CAST(X'4352454154452054524947474552206c697374696e675f7265706c61795f72756e5f6974656d735f636f6d706c657465645f696d6d757461626c655f7570646174650a4245464f524520555044415445' AS TEXT) ||
     CAST(X'204f4e206c697374696e675f7265706c61795f72756e5f6974656d730a5748454e204f4c442e6d6174657269616c697a6174696f6e5f7374617465203d2027737563636565646564270a424547494e0a' AS TEXT) ||
     CAST(X'202053454c4543542052414953452841424f52542c2027636f6d706c65746564207265706c6179206974656d20697320696d6d757461626c6527293b0a454e44' AS TEXT)),
    ('listing_replay_run_items_completed_immutable_delete',
     CAST(X'4352454154452054524947474552206c697374696e675f7265706c61795f72756e5f6974656d735f636f6d706c657465645f696d6d757461626c655f64656c6574650a4245464f52452044454c455445' AS TEXT) ||
     CAST(X'204f4e206c697374696e675f7265706c61795f72756e5f6974656d730a5748454e204f4c442e6d6174657269616c697a6174696f6e5f7374617465203d2027737563636565646564270a424547494e0a' AS TEXT) ||
     CAST(X'202053454c4543542052414953452841424f52542c2027636f6d706c65746564207265706c6179206974656d20697320696d6d757461626c6527293b0a454e44' AS TEXT)),
    ('plugin_submission_materialization_receipts_immutable_update',
     CAST(X'435245415445205452494747455220706c7567696e5f7375626d697373696f6e5f6d6174657269616c697a6174696f6e5f72656365697074735f696d6d757461626c655f7570646174650a4245464f52' AS TEXT) ||
     CAST(X'4520555044415445204f4e20706c7567696e5f7375626d697373696f6e5f6d6174657269616c697a6174696f6e5f72656365697074730a424547494e0a202053454c4543542052414953452841424f52' AS TEXT) ||
     CAST(X'542c20277265706c6179206d6174657269616c697a6174696f6e207265636569707420697320696d6d757461626c6527293b0a454e44' AS TEXT)),
    ('plugin_submission_materialization_receipts_immutable_delete',
     CAST(X'435245415445205452494747455220706c7567696e5f7375626d697373696f6e5f6d6174657269616c697a6174696f6e5f72656365697074735f696d6d757461626c655f64656c6574650a4245464f52' AS TEXT) ||
     CAST(X'452044454c455445204f4e20706c7567696e5f7375626d697373696f6e5f6d6174657269616c697a6174696f6e5f72656365697074730a424547494e0a202053454c4543542052414953452841424f52' AS TEXT) ||
     CAST(X'542c20277265706c6179206d6174657269616c697a6174696f6e207265636569707420697320696d6d757461626c6527293b0a454e44' AS TEXT)),
    ('plugin_submissions_replay_checkpoint_immutable',
     CAST(X'435245415445205452494747455220706c7567696e5f7375626d697373696f6e735f7265706c61795f636865636b706f696e745f696d6d757461626c650a4245464f524520555044415445204f4e2070' AS TEXT) ||
     CAST(X'6c7567696e5f7375626d697373696f6e730a5748454e20280a202045584953545320280a2020202053454c45435420312046524f4d206c697374696e675f7265706c61795f72756e5f6974656d732069' AS TEXT) ||
     CAST(X'74656d0a202020205748455245206974656d2e706c7567696e5f7375626d697373696f6e5f6964203d204f4c442e69640a202020202020414e44206974656d2e65787472616374696f6e5f7374617465' AS TEXT) ||
     CAST(X'203d2027737563636565646564270a2020290a20204f522045584953545320280a2020202053454c45435420312046524f4d20706c7567696e5f7375626d697373696f6e5f6d6174657269616c697a61' AS TEXT) ||
     CAST(X'74696f6e5f726563656970747320726563656970740a20202020574845524520726563656970742e706c7567696e5f7375626d697373696f6e5f6964203d204f4c442e69640a2020290a2920414e4420' AS TEXT) ||
     CAST(X'280a20204e4f5420284e45572e6964204953204f4c442e6964290a20204f52204e4f5420284e45572e757365725f6964204953204f4c442e757365725f6964290a20204f52204e4f5420284e45572e70' AS TEXT) ||
     CAST(X'6c7567696e5f696e7374616c6c5f6964204953204f4c442e706c7567696e5f696e7374616c6c5f6964290a20204f52204e4f5420284e45572e736f757263655f75726c204953204f4c442e736f757263' AS TEXT) ||
     CAST(X'655f75726c290a20204f52204e4f5420284e45572e7375626d69747465645f6174204953204f4c442e7375626d69747465645f6174290a20204f52204e4f5420284e45572e72656e64657265645f6874' AS TEXT) ||
     CAST(X'6d6c204953204f4c442e72656e64657265645f68746d6c290a20204f52204e4f5420284e45572e72656e64657265645f68746d6c5f736861323536204953204f4c442e72656e64657265645f68746d6c' AS TEXT) ||
     CAST(X'5f736861323536290a20204f52204e4f5420284e45572e7369676e61747572655f626173653634204953204f4c442e7369676e61747572655f626173653634290a20204f52204e4f5420284e45572e65' AS TEXT) ||
     CAST(X'78747261637465645f6c697374696e675f6a736f6e204953204f4c442e6578747261637465645f6c697374696e675f6a736f6e290a20204f52204e4f5420284e45572e65787472616374696f6e5f6572' AS TEXT) ||
     CAST(X'726f72204953204f4c442e65787472616374696f6e5f6572726f72290a20204f52204e4f5420280a202020204e45572e63616e6f6e6963616c5f6c697374696e675f6964204953204f4c442e63616e6f' AS TEXT) ||
     CAST(X'6e6963616c5f6c697374696e675f69640a202020204f5220280a2020202020204f4c442e63616e6f6e6963616c5f6c697374696e675f6964204953204e554c4c0a202020202020414e44204e45572e63' AS TEXT) ||
     CAST(X'616e6f6e6963616c5f6c697374696e675f6964204953204e4f54204e554c4c0a202020202020414e44204e4f542045584953545320280a202020202020202053454c45435420312046524f4d20706c75' AS TEXT) ||
     CAST(X'67696e5f7375626d697373696f6e5f6d6174657269616c697a6174696f6e5f726563656970747320726563656970740a2020202020202020574845524520726563656970742e706c7567696e5f737562' AS TEXT) ||
     CAST(X'6d697373696f6e5f6964203d204f4c442e69640a202020202020290a202020202020414e442045584953545320280a202020202020202053454c45435420312046524f4d2061697263726166745f7361' AS TEXT) ||
     CAST(X'6c655f6c697374696e6773206c697374696e670a20202020202020205748455245206c697374696e672e6964203d204e45572e63616e6f6e6963616c5f6c697374696e675f69640a2020202020202020' AS TEXT) ||
     CAST(X'2020414e44206c697374696e672e637265617465645f62795f757365725f6964203d204f4c442e757365725f69640a20202020202020202020414e44206c697374696e672e736f757263655f75726c20' AS TEXT) ||
     CAST(X'3d204f4c442e736f757263655f75726c0a202020202020290a20202020290a2020290a290a424547494e0a202053454c4543542052414953452841424f52542c20277265706c617920636865636b706f' AS TEXT) ||
     CAST(X'696e74206361707475726520697320696d6d757461626c6527293b0a454e44' AS TEXT)),
    ('plugin_installs_replay_identity_immutable',
     CAST(X'435245415445205452494747455220706c7567696e5f696e7374616c6c735f7265706c61795f6964656e746974795f696d6d757461626c650a4245464f524520555044415445204f4e20706c7567696e' AS TEXT) ||
     CAST(X'5f696e7374616c6c730a5748454e2045584953545320280a202053454c45435420310a202046524f4d20706c7567696e5f7375626d697373696f6e73207375626d697373696f6e0a2020574845524520' AS TEXT) ||
     CAST(X'7375626d697373696f6e2e706c7567696e5f696e7374616c6c5f6964203d204f4c442e69640a20202020414e4420280a20202020202045584953545320280a202020202020202053454c454354203120' AS TEXT) ||
     CAST(X'46524f4d206c697374696e675f7265706c61795f72756e5f6974656d73206974656d0a20202020202020205748455245206974656d2e706c7567696e5f7375626d697373696f6e5f6964203d20737562' AS TEXT) ||
     CAST(X'6d697373696f6e2e69640a20202020202020202020414e44206974656d2e65787472616374696f6e5f7374617465203d2027737563636565646564270a202020202020290a2020202020204f52204558' AS TEXT) ||
     CAST(X'4953545320280a202020202020202053454c45435420312046524f4d20706c7567696e5f7375626d697373696f6e5f6d6174657269616c697a6174696f6e5f726563656970747320726563656970740a' AS TEXT) ||
     CAST(X'2020202020202020574845524520726563656970742e706c7567696e5f7375626d697373696f6e5f6964203d207375626d697373696f6e2e69640a202020202020290a20202020290a2920414e442028' AS TEXT) ||
     CAST(X'0a20204e4f5420284e45572e6964204953204f4c442e6964290a20204f52204e4f5420284e45572e757365725f6964204953204f4c442e757365725f6964290a20204f52204e4f5420284e45572e7075' AS TEXT) ||
     CAST(X'626c69635f6b65795f626173653634204953204f4c442e7075626c69635f6b65795f626173653634290a20204f52204e4f5420284e45572e637265617465645f6174204953204f4c442e637265617465' AS TEXT) ||
     CAST(X'645f6174290a20204f52204e4f5420280a202020204e45572e7265766f6b65645f6174204953204f4c442e7265766f6b65645f61740a202020204f5220280a2020202020204f4c442e7265766f6b6564' AS TEXT) ||
     CAST(X'5f6174204953204e554c4c0a202020202020414e44204e45572e7265766f6b65645f6174204953204e4f54204e554c4c0a202020202020414e44206a756c69616e646179284e45572e7265766f6b6564' AS TEXT) ||
     CAST(X'5f617429204953204e4f54204e554c4c0a202020202020414e44204e4f542045584953545320280a202020202020202053454c45435420310a202020202020202046524f4d20706c7567696e5f737562' AS TEXT) ||
     CAST(X'6d697373696f6e73207375626d697373696f6e0a20202020202020205748455245207375626d697373696f6e2e706c7567696e5f696e7374616c6c5f6964203d204f4c442e69640a2020202020202020' AS TEXT) ||
     CAST(X'2020414e4420280a20202020202020202020202045584953545320280a202020202020202020202020202053454c45435420312046524f4d206c697374696e675f7265706c61795f72756e5f6974656d' AS TEXT) ||
     CAST(X'73206974656d0a20202020202020202020202020205748455245206974656d2e706c7567696e5f7375626d697373696f6e5f6964203d207375626d697373696f6e2e69640a2020202020202020202020' AS TEXT) ||
     CAST(X'2020202020414e44206974656d2e65787472616374696f6e5f7374617465203d2027737563636565646564270a202020202020202020202020290a2020202020202020202020204f5220455849535453' AS TEXT) ||
     CAST(X'20280a202020202020202020202020202053454c45435420312046524f4d20706c7567696e5f7375626d697373696f6e5f6d6174657269616c697a6174696f6e5f726563656970747320726563656970' AS TEXT) ||
     CAST(X'740a2020202020202020202020202020574845524520726563656970742e706c7567696e5f7375626d697373696f6e5f6964203d207375626d697373696f6e2e69640a20202020202020202020202029' AS TEXT) ||
     CAST(X'0a20202020202020202020290a20202020202020202020414e4420280a2020202020202020202020206a756c69616e646179287375626d697373696f6e2e7375626d69747465645f617429204953204e' AS TEXT) ||
     CAST(X'554c4c0a2020202020202020202020204f52206a756c69616e646179287375626d697373696f6e2e7375626d69747465645f617429203e206a756c69616e646179284e45572e7265766f6b65645f6174' AS TEXT) ||
     CAST(X'290a20202020202020202020290a202020202020290a20202020290a2020290a290a424547494e0a202053454c4543542052414953452841424f52542c20277265706c617920636865636b706f696e74' AS TEXT) ||
     CAST(X'20706c7567696e206964656e7469747920697320696d6d757461626c6527293b0a454e44' AS TEXT)),
    ('listing_replay_submission_inventory_lock_no_delete',
     'CREATE TRIGGER listing_replay_submission_inventory_lock_no_delete
BEFORE DELETE ON listing_replay_submission_inventory_lock
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is a protected singleton'');
END'),
    ('listing_replay_submission_inventory_lock_no_identity_update',
     'CREATE TRIGGER listing_replay_submission_inventory_lock_no_identity_update
BEFORE UPDATE ON listing_replay_submission_inventory_lock
WHEN NOT (NEW.singleton_id IS OLD.singleton_id)
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is a protected singleton'');
END'),
    ('listing_replay_submission_inventory_lock_no_insert',
     'CREATE TRIGGER listing_replay_submission_inventory_lock_no_insert
BEFORE INSERT ON listing_replay_submission_inventory_lock
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is a protected singleton'');
END'),
    ('plugin_installs_active_replay_capture_identity_frozen_delete',
     'CREATE TRIGGER plugin_installs_active_replay_capture_identity_frozen_delete
BEFORE DELETE ON plugin_installs
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission capture identity is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND EXISTS (
    SELECT 1 FROM plugin_submissions WHERE plugin_install_id = OLD.id
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('plugin_installs_active_replay_capture_identity_frozen_insert',
     'CREATE TRIGGER plugin_installs_active_replay_capture_identity_frozen_insert
BEFORE INSERT ON plugin_installs
WHEN EXISTS (
  SELECT 1 FROM plugin_submissions submission
  WHERE submission.plugin_install_id = NEW.id
)
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission capture identity is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('plugin_installs_active_replay_capture_identity_frozen_update',
     'CREATE TRIGGER plugin_installs_active_replay_capture_identity_frozen_update
BEFORE UPDATE ON plugin_installs
WHEN (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.public_key_base64 IS OLD.public_key_base64)
  OR NOT (NEW.created_at IS OLD.created_at)
  OR NOT (NEW.revoked_at IS OLD.revoked_at)
)
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission capture identity is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND EXISTS (
    SELECT 1 FROM plugin_submissions
    WHERE plugin_install_id IN (OLD.id, NEW.id)
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('plugin_submissions_active_replay_membership_frozen_delete',
     'CREATE TRIGGER plugin_submissions_active_replay_membership_frozen_delete
BEFORE DELETE ON plugin_submissions
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission membership is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('plugin_submissions_active_replay_membership_frozen_insert',
     'CREATE TRIGGER plugin_submissions_active_replay_membership_frozen_insert
BEFORE INSERT ON plugin_submissions
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission membership is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('plugin_submissions_active_replay_membership_frozen_update',
     'CREATE TRIGGER plugin_submissions_active_replay_membership_frozen_update
BEFORE UPDATE ON plugin_submissions
WHEN NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.plugin_install_id IS OLD.plugin_install_id)
  OR NOT (NEW.source_url IS OLD.source_url)
  OR NOT (NEW.submitted_at IS OLD.submitted_at)
  OR NOT (NEW.rendered_html IS OLD.rendered_html)
  OR NOT (NEW.rendered_html_sha256 IS OLD.rendered_html_sha256)
  OR NOT (NEW.signature_base64 IS OLD.signature_base64)
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission membership is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('users_active_replay_capture_identity_frozen_delete',
     'CREATE TRIGGER users_active_replay_capture_identity_frozen_delete
BEFORE DELETE ON users
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission capture identity is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND EXISTS (
    SELECT 1 FROM plugin_submissions WHERE user_id = OLD.id
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('users_active_replay_capture_identity_frozen_insert',
     'CREATE TRIGGER users_active_replay_capture_identity_frozen_insert
BEFORE INSERT ON users
WHEN EXISTS (
  SELECT 1 FROM users existing
  JOIN plugin_submissions submission ON submission.user_id = existing.id
  WHERE existing.id = NEW.id
     OR existing.email = NEW.email
     OR existing.auth_subject = NEW.auth_subject
)
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission capture identity is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('users_active_replay_capture_identity_frozen_update',
     'CREATE TRIGGER users_active_replay_capture_identity_frozen_update
BEFORE UPDATE ON users
WHEN (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.email IS OLD.email)
  OR NOT (NEW.display_name IS OLD.display_name)
  OR NOT (NEW.auth_provider IS OLD.auth_provider)
  OR NOT (NEW.auth_subject IS OLD.auth_subject)
)
BEGIN
  SELECT RAISE(ABORT, ''replay submission inventory lock is invalid'')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, ''plugin submission capture identity is frozen by active replay'')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND (
    EXISTS (SELECT 1 FROM plugin_submissions WHERE user_id = OLD.id)
    OR EXISTS (
      SELECT 1
      FROM users existing
      JOIN plugin_submissions submission ON submission.user_id = existing.id
      WHERE existing.id = NEW.id
         OR existing.email = NEW.email
         OR existing.auth_subject = NEW.auth_subject
    )
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END'),
    ('uq_aircraft_sale_listings_owner_source',
     CAST(X'43524541544520554e4951554520494e4445582075715f61697263726166745f73616c655f6c697374696e67735f6f776e65725f736f757263650a20204f4e2061697263726166745f73616c655f6c69' AS TEXT) ||
     CAST(X'7374696e67732028637265617465645f62795f757365725f69642c20736f757263655f75726c290a2020574845524520736f757263655f75726c204953204e4f54204e554c4c20414e44206c656e6774' AS TEXT) ||
     CAST(X'68287472696d28736f757263655f75726c2929203e2030' AS TEXT))
),
replay_contract_guard(accepted) AS (
  SELECT
  (
    NOT EXISTS (
      SELECT 1 FROM schema_migration_contracts
      WHERE migration_name = '20260819_listing_replay_runs'
    )
    AND NOT EXISTS (
      SELECT 1 FROM sqlite_schema
      WHERE name IN (
        'listing_replay_runs', 'listing_replay_run_items',
        'listing_replay_submission_inventory_lock',
        'plugin_submission_materialization_receipts',
        'idx_listing_replay_runs_one_running',
        'idx_listing_replay_run_items_phase',
        'uq_aircraft_sale_listings_owner_source',
        'listing_replay_run_items_checkpoint_exact_insert',
        'listing_replay_run_items_checkpoint_exact_update',
        'listing_replay_run_items_completed_immutable_update',
        'listing_replay_run_items_completed_immutable_delete',
        'plugin_submission_materialization_receipts_immutable_update',
        'plugin_submission_materialization_receipts_immutable_delete',
        'plugin_submissions_active_replay_membership_frozen_delete',
        'plugin_submissions_active_replay_membership_frozen_insert',
        'plugin_submissions_active_replay_membership_frozen_update',
        'listing_replay_submission_inventory_lock_no_insert',
        'listing_replay_submission_inventory_lock_no_delete',
        'listing_replay_submission_inventory_lock_no_identity_update',
        'users_active_replay_capture_identity_frozen_insert',
        'users_active_replay_capture_identity_frozen_update',
        'users_active_replay_capture_identity_frozen_delete',
        'plugin_installs_active_replay_capture_identity_frozen_insert',
        'plugin_installs_active_replay_capture_identity_frozen_update',
        'plugin_installs_active_replay_capture_identity_frozen_delete',
        'plugin_submissions_replay_checkpoint_immutable',
        'plugin_installs_replay_identity_immutable'
      )
    )
  )
  OR
  (
    EXISTS (
      SELECT 1 FROM schema_migration_contracts
      WHERE migration_name = '20260819_listing_replay_runs'
        AND contract_version = 1
        AND contract_fingerprint =
          '3e7c0b39b66e681be397bddbc943c75793b18bac71eacc7324b08a067ef3ff01'
    )
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name = 'listing_replay_runs'
           AND sql = 'CREATE TABLE listing_replay_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manifest_sha256 TEXT NOT NULL UNIQUE,
  manifest_capture_count INTEGER NOT NULL CHECK (manifest_capture_count > 0),
  status TEXT NOT NULL DEFAULT ''queued''
    CHECK (status IN (''queued'', ''running'', ''completed'')),
  active_phase TEXT CHECK (active_phase IN (''extraction'', ''materialization'')),
  owner_token TEXT,
  heartbeat_at_epoch_seconds INTEGER,
  started_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(manifest_sha256) = 64),
  CHECK (manifest_sha256 = lower(manifest_sha256)),
  CHECK (manifest_sha256 NOT GLOB ''*[^0-9a-f]*''),
  CHECK (owner_token IS NULL OR length(trim(owner_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = ''running'' AND active_phase IS NOT NULL AND owner_token IS NOT NULL
      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status = ''queued'' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)
    OR
    (status = ''completed'' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)
  )
)') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table'
           AND name = 'listing_replay_submission_inventory_lock'
           AND sql = 'CREATE TABLE listing_replay_submission_inventory_lock (
  singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
  active_run_id INTEGER UNIQUE
    REFERENCES listing_replay_runs(id) ON DELETE RESTRICT,
  concurrency_token INTEGER NOT NULL DEFAULT 0 CHECK (concurrency_token >= 0)
)') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'index' AND name = 'idx_listing_replay_runs_one_running'
           AND sql = 'CREATE UNIQUE INDEX idx_listing_replay_runs_one_running
  ON listing_replay_runs (status) WHERE status = ''running''') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name = 'listing_replay_run_items'
           AND sql = 'CREATE TABLE listing_replay_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES listing_replay_runs(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK (position >= 0),
  expected_rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT,
  extracted_listing_json TEXT,
  extraction_state TEXT NOT NULL DEFAULT ''queued''
    CHECK (extraction_state IN (''queued'', ''running'', ''succeeded'', ''rejected'', ''failed'')),
  materialization_state TEXT NOT NULL DEFAULT ''blocked''
    CHECK (materialization_state IN (''blocked'', ''queued'', ''running'', ''succeeded'', ''rejected'', ''failed'')),
  resulting_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  terminal_rejection_phase TEXT
    CHECK (terminal_rejection_phase IN (''extraction'', ''materialization'')),
  terminal_rejection_stage TEXT CHECK (terminal_rejection_stage IN (
    ''capture_admission'', ''faa_aircraft_admission''
  )),
  terminal_rejection_reason_code TEXT CHECK (terminal_rejection_reason_code IN (
    ''capture_authentication_failed'', ''capture_not_found'', ''capture_validation_failed'',
    ''missing_registration'', ''non_n_registration'',
    ''invalid_n_number'', ''serial_conflict''
  )),
  last_failure_phase TEXT CHECK (last_failure_phase IN (''extraction'', ''materialization'')),
  last_failure_reason_code TEXT
    CHECK (last_failure_reason_code IN (
      ''database_error'', ''operation_failed'', ''faa_lookup_failed'', ''faa_listing_not_found'',
      ''faa_registry_snapshot_unavailable'', ''faa_registration_not_found'',
      ''faa_registration_not_covered'', ''faa_ambiguous_registration'',
      ''faa_registry_aircraft_identity_unavailable'', ''faa_aircraft_manufacturer_mismatch'',
      ''faa_aircraft_model_mismatch'', ''faa_canonical_identity_assignment_missing'',
      ''faa_canonical_identity_assignment_mismatch''
    )),
  extraction_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (extraction_attempt_count >= 0),
  materialization_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (materialization_attempt_count >= 0),
  extraction_started_at TEXT,
  extraction_completed_at TEXT,
  materialization_started_at TEXT,
  materialization_completed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (run_id, position),
  UNIQUE (run_id, plugin_submission_id),
  CHECK (length(expected_rendered_html_sha256) = 64),
  CHECK (expected_rendered_html_sha256 = lower(expected_rendered_html_sha256)),
  CHECK (expected_rendered_html_sha256 NOT GLOB ''*[^0-9a-f]*''),
  CHECK (extracted_listing_sha256 IS NULL OR (
    length(extracted_listing_sha256) = 64
    AND extracted_listing_sha256 = lower(extracted_listing_sha256)
    AND extracted_listing_sha256 NOT GLOB ''*[^0-9a-f]*''
  )),
  CHECK (
    (extraction_state = ''rejected'' AND materialization_state = ''blocked''
      AND terminal_rejection_phase = ''extraction''
      AND terminal_rejection_stage = ''capture_admission''
      AND terminal_rejection_reason_code IN (
        ''capture_authentication_failed'', ''capture_not_found'', ''capture_validation_failed''
      ))
    OR
    (extraction_state = ''succeeded'' AND materialization_state = ''rejected''
      AND terminal_rejection_phase = ''materialization''
      AND (
        (terminal_rejection_stage = ''capture_admission''
          AND terminal_rejection_reason_code IN (
            ''capture_authentication_failed'', ''capture_not_found'', ''capture_validation_failed''
          ))
        OR
        (terminal_rejection_stage = ''faa_aircraft_admission''
          AND terminal_rejection_reason_code IN (
            ''missing_registration'', ''non_n_registration'', ''invalid_n_number'', ''serial_conflict''
          ))
      ))
    OR
    (extraction_state <> ''rejected'' AND materialization_state <> ''rejected''
      AND terminal_rejection_phase IS NULL AND terminal_rejection_stage IS NULL
      AND terminal_rejection_reason_code IS NULL)
  ),
  CHECK (
    (extraction_state = ''failed'' AND materialization_state = ''blocked''
      AND last_failure_phase = ''extraction''
      AND last_failure_reason_code IN (''database_error'', ''operation_failed''))
    OR
    (extraction_state = ''succeeded'' AND materialization_state = ''failed''
      AND last_failure_phase = ''materialization'' AND last_failure_reason_code IS NOT NULL)
    OR
    (extraction_state <> ''failed'' AND materialization_state <> ''failed''
      AND last_failure_phase IS NULL AND last_failure_reason_code IS NULL)
  ),
  CHECK ((materialization_state = ''succeeded'') = (resulting_listing_id IS NOT NULL)),
  CHECK ((extraction_state = ''succeeded'') = (extracted_listing_sha256 IS NOT NULL)),
  CHECK ((extraction_state = ''succeeded'') = (extracted_listing_json IS NOT NULL)),
  CHECK (extraction_state = ''succeeded'' OR materialization_state = ''blocked''),
  CHECK (extraction_state <> ''running'' OR extraction_started_at IS NOT NULL),
  CHECK (materialization_state <> ''running'' OR materialization_started_at IS NOT NULL)
)') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'index' AND name = 'idx_listing_replay_run_items_phase'
           AND sql = 'CREATE INDEX idx_listing_replay_run_items_phase
  ON listing_replay_run_items (run_id, extraction_state, materialization_state, position)') = 1
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'table' AND name = 'plugin_submission_materialization_receipts'
           AND sql = 'CREATE TABLE plugin_submission_materialization_receipts (
  plugin_submission_id INTEGER PRIMARY KEY
    REFERENCES plugin_submissions(id) ON DELETE CASCADE,
  aircraft_sale_listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT NOT NULL,
  completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(rendered_html_sha256) = 64),
  CHECK (rendered_html_sha256 = lower(rendered_html_sha256)),
  CHECK (rendered_html_sha256 NOT GLOB ''*[^0-9a-f]*''),
  CHECK (length(extracted_listing_sha256) = 64),
  CHECK (extracted_listing_sha256 = lower(extracted_listing_sha256)),
  CHECK (extracted_listing_sha256 NOT GLOB ''*[^0-9a-f]*'')
)') = 1
    AND NOT EXISTS (
      SELECT 1
      FROM expected_replay_guard_definitions expected
      LEFT JOIN sqlite_schema actual
        ON actual.name = expected.name
       AND actual.type IN ('trigger', 'index')
      WHERE actual.name IS NULL OR actual.sql <> expected.definition
    )
    AND NOT EXISTS (
      SELECT 1 FROM sqlite_schema
      WHERE tbl_name IN (
        'listing_replay_runs', 'listing_replay_run_items',
        'listing_replay_submission_inventory_lock',
        'plugin_submission_materialization_receipts'
      )
        AND (
          (
            type = 'trigger'
            AND name NOT IN (
              'listing_replay_run_items_checkpoint_exact_insert',
              'listing_replay_run_items_checkpoint_exact_update',
              'listing_replay_run_items_completed_immutable_update',
              'listing_replay_run_items_completed_immutable_delete',
              'plugin_submission_materialization_receipts_immutable_update',
              'plugin_submission_materialization_receipts_immutable_delete',
              'listing_replay_submission_inventory_lock_no_insert',
              'listing_replay_submission_inventory_lock_no_delete',
              'listing_replay_submission_inventory_lock_no_identity_update'
            )
          )
          OR (
            type = 'index'
            AND name NOT IN (
              'idx_listing_replay_runs_one_running',
              'idx_listing_replay_run_items_phase',
              'sqlite_autoindex_listing_replay_runs_1',
              'sqlite_autoindex_listing_replay_run_items_1',
              'sqlite_autoindex_listing_replay_run_items_2',
              'sqlite_autoindex_listing_replay_submission_inventory_lock_1',
              'sqlite_autoindex_plugin_submission_materialization_receipts_1'
            )
          )
        )
    )
    AND NOT EXISTS (
      SELECT 1 FROM sqlite_schema
      WHERE type = 'trigger'
        AND tbl_name IN ('plugin_submissions', 'plugin_installs', 'users')
        AND NOT (
          (
            tbl_name = 'plugin_submissions'
            AND name IN (
              'plugin_submissions_active_replay_membership_frozen_delete',
              'plugin_submissions_active_replay_membership_frozen_insert',
              'plugin_submissions_active_replay_membership_frozen_update',
              'plugin_submissions_replay_checkpoint_immutable',
              'listing_avionics_authorizations_invalidate_capture_delete',
              'listing_avionics_authorizations_invalidate_capture_update'
            )
          )
          OR (
            tbl_name = 'plugin_installs'
            AND name IN (
              'listing_avionics_authorizations_invalidate_install_provenance',
              'plugin_installs_replay_identity_immutable',
              'plugin_installs_active_replay_capture_identity_frozen_insert',
              'plugin_installs_active_replay_capture_identity_frozen_update',
              'plugin_installs_active_replay_capture_identity_frozen_delete'
            )
          )
          OR (
            tbl_name = 'users'
            AND name IN (
              'users_active_replay_capture_identity_frozen_insert',
              'users_active_replay_capture_identity_frozen_update',
              'users_active_replay_capture_identity_frozen_delete'
            )
          )
        )
    )
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE type = 'index' AND tbl_name IN (
           'listing_replay_runs', 'listing_replay_run_items',
           'listing_replay_submission_inventory_lock',
           'plugin_submission_materialization_receipts'
         )) = 7
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE name IN (
           'uq_aircraft_sale_listings_owner_source',
           'listing_replay_run_items_checkpoint_exact_insert',
           'listing_replay_run_items_checkpoint_exact_update',
           'listing_replay_run_items_completed_immutable_update',
           'listing_replay_run_items_completed_immutable_delete',
           'plugin_submission_materialization_receipts_immutable_update',
           'plugin_submission_materialization_receipts_immutable_delete',
           'plugin_submissions_active_replay_membership_frozen_delete',
           'plugin_submissions_active_replay_membership_frozen_insert',
           'plugin_submissions_active_replay_membership_frozen_update',
           'listing_replay_submission_inventory_lock_no_insert',
           'listing_replay_submission_inventory_lock_no_delete',
           'listing_replay_submission_inventory_lock_no_identity_update',
           'users_active_replay_capture_identity_frozen_insert',
           'users_active_replay_capture_identity_frozen_update',
           'users_active_replay_capture_identity_frozen_delete',
           'plugin_installs_active_replay_capture_identity_frozen_insert',
           'plugin_installs_active_replay_capture_identity_frozen_update',
           'plugin_installs_active_replay_capture_identity_frozen_delete',
           'plugin_submissions_replay_checkpoint_immutable',
           'plugin_installs_replay_identity_immutable'
         )) = 21
    AND (SELECT COUNT(*) FROM sqlite_schema
         WHERE name IN (
           'listing_replay_runs', 'listing_replay_run_items',
           'listing_replay_submission_inventory_lock',
           'plugin_submission_materialization_receipts',
           'idx_listing_replay_runs_one_running',
           'idx_listing_replay_run_items_phase'
         )) = 6
  )
),
duplicate_guard_rows(row_number) AS (
  VALUES (1), (2)
)
INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
)
SELECT '__listing_replay_runs_contract_guard__', 1,
       '0000000000000000000000000000000000000000000000000000000000000000',
       'contract-guard'
FROM replay_contract_guard
CROSS JOIN duplicate_guard_rows
WHERE NOT accepted;

CREATE TABLE IF NOT EXISTS listing_replay_runs (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  manifest_sha256 TEXT NOT NULL UNIQUE,
  manifest_capture_count INTEGER NOT NULL CHECK (manifest_capture_count > 0),
  status TEXT NOT NULL DEFAULT 'queued'
    CHECK (status IN ('queued', 'running', 'completed')),
  active_phase TEXT CHECK (active_phase IN ('extraction', 'materialization')),
  owner_token TEXT,
  heartbeat_at_epoch_seconds INTEGER,
  started_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  completed_at TEXT,
  CHECK (length(manifest_sha256) = 64),
  CHECK (manifest_sha256 = lower(manifest_sha256)),
  CHECK (manifest_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (owner_token IS NULL OR length(trim(owner_token)) BETWEEN 1 AND 200),
  CHECK (
    (status = 'running' AND active_phase IS NOT NULL AND owner_token IS NOT NULL
      AND heartbeat_at_epoch_seconds IS NOT NULL AND started_at IS NOT NULL
      AND completed_at IS NULL)
    OR
    (status = 'queued' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NULL)
    OR
    (status = 'completed' AND active_phase IS NULL AND owner_token IS NULL
      AND heartbeat_at_epoch_seconds IS NULL AND completed_at IS NOT NULL)
  )
);

CREATE TABLE IF NOT EXISTS listing_replay_submission_inventory_lock (
  singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
  active_run_id INTEGER UNIQUE
    REFERENCES listing_replay_runs(id) ON DELETE RESTRICT,
  concurrency_token INTEGER NOT NULL DEFAULT 0 CHECK (concurrency_token >= 0)
);

INSERT INTO listing_replay_submission_inventory_lock (
  singleton_id, active_run_id, concurrency_token
) SELECT 1, NULL, 0
WHERE NOT EXISTS (
  SELECT 1 FROM listing_replay_submission_inventory_lock
)
AND NOT EXISTS (
  SELECT 1 FROM schema_migration_contracts
  WHERE migration_name = '20260819_listing_replay_runs'
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_listing_replay_runs_one_running
  ON listing_replay_runs (status) WHERE status = 'running';

CREATE TRIGGER IF NOT EXISTS listing_replay_submission_inventory_lock_no_insert
BEFORE INSERT ON listing_replay_submission_inventory_lock
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is a protected singleton');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_submission_inventory_lock_no_delete
BEFORE DELETE ON listing_replay_submission_inventory_lock
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is a protected singleton');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_submission_inventory_lock_no_identity_update
BEFORE UPDATE ON listing_replay_submission_inventory_lock
WHEN NOT (NEW.singleton_id IS OLD.singleton_id)
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is a protected singleton');
END;

CREATE TRIGGER IF NOT EXISTS plugin_submissions_active_replay_membership_frozen_insert
BEFORE INSERT ON plugin_submissions
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission membership is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS plugin_submissions_active_replay_membership_frozen_delete
BEFORE DELETE ON plugin_submissions
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission membership is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS plugin_submissions_active_replay_membership_frozen_update
BEFORE UPDATE ON plugin_submissions
WHEN NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.plugin_install_id IS OLD.plugin_install_id)
  OR NOT (NEW.source_url IS OLD.source_url)
  OR NOT (NEW.submitted_at IS OLD.submitted_at)
  OR NOT (NEW.rendered_html IS OLD.rendered_html)
  OR NOT (NEW.rendered_html_sha256 IS OLD.rendered_html_sha256)
  OR NOT (NEW.signature_base64 IS OLD.signature_base64)
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission membership is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS users_active_replay_capture_identity_frozen_update
BEFORE UPDATE ON users
WHEN (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.email IS OLD.email)
  OR NOT (NEW.display_name IS OLD.display_name)
  OR NOT (NEW.auth_provider IS OLD.auth_provider)
  OR NOT (NEW.auth_subject IS OLD.auth_subject)
)
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission capture identity is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND (
    EXISTS (SELECT 1 FROM plugin_submissions WHERE user_id = OLD.id)
    OR EXISTS (
      SELECT 1
      FROM users existing
      JOIN plugin_submissions submission ON submission.user_id = existing.id
      WHERE existing.id = NEW.id
         OR existing.email = NEW.email
         OR existing.auth_subject = NEW.auth_subject
    )
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS users_active_replay_capture_identity_frozen_insert
BEFORE INSERT ON users
WHEN EXISTS (
  SELECT 1 FROM users existing
  JOIN plugin_submissions submission ON submission.user_id = existing.id
  WHERE existing.id = NEW.id
     OR existing.email = NEW.email
     OR existing.auth_subject = NEW.auth_subject
)
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission capture identity is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS users_active_replay_capture_identity_frozen_delete
BEFORE DELETE ON users
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission capture identity is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND EXISTS (
    SELECT 1 FROM plugin_submissions WHERE user_id = OLD.id
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS plugin_installs_active_replay_capture_identity_frozen_update
BEFORE UPDATE ON plugin_installs
WHEN (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.public_key_base64 IS OLD.public_key_base64)
  OR NOT (NEW.created_at IS OLD.created_at)
  OR NOT (NEW.revoked_at IS OLD.revoked_at)
)
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission capture identity is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND EXISTS (
    SELECT 1 FROM plugin_submissions
    WHERE plugin_install_id IN (OLD.id, NEW.id)
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS plugin_installs_active_replay_capture_identity_frozen_insert
BEFORE INSERT ON plugin_installs
WHEN EXISTS (
  SELECT 1 FROM plugin_submissions submission
  WHERE submission.plugin_install_id = NEW.id
)
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission capture identity is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL;
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TRIGGER IF NOT EXISTS plugin_installs_active_replay_capture_identity_frozen_delete
BEFORE DELETE ON plugin_installs
BEGIN
  SELECT RAISE(ABORT, 'replay submission inventory lock is invalid')
  WHERE (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) <> 1;
  SELECT RAISE(ABORT, 'plugin submission capture identity is frozen by active replay')
  WHERE (
    SELECT active_run_id FROM listing_replay_submission_inventory_lock
    WHERE singleton_id = 1
  ) IS NOT NULL AND EXISTS (
    SELECT 1 FROM plugin_submissions WHERE plugin_install_id = OLD.id
  );
  UPDATE listing_replay_submission_inventory_lock
  SET concurrency_token = concurrency_token + 1 WHERE singleton_id = 1;
END;

CREATE TABLE IF NOT EXISTS listing_replay_run_items (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  run_id INTEGER NOT NULL REFERENCES listing_replay_runs(id) ON DELETE CASCADE,
  plugin_submission_id INTEGER NOT NULL
    REFERENCES plugin_submissions(id) ON DELETE RESTRICT,
  position INTEGER NOT NULL CHECK (position >= 0),
  expected_rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT,
  extracted_listing_json TEXT,
  extraction_state TEXT NOT NULL DEFAULT 'queued'
    CHECK (extraction_state IN ('queued', 'running', 'succeeded', 'rejected', 'failed')),
  materialization_state TEXT NOT NULL DEFAULT 'blocked'
    CHECK (materialization_state IN ('blocked', 'queued', 'running', 'succeeded', 'rejected', 'failed')),
  resulting_listing_id INTEGER
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  terminal_rejection_phase TEXT
    CHECK (terminal_rejection_phase IN ('extraction', 'materialization')),
  terminal_rejection_stage TEXT CHECK (terminal_rejection_stage IN (
    'capture_admission', 'faa_aircraft_admission'
  )),
  terminal_rejection_reason_code TEXT CHECK (terminal_rejection_reason_code IN (
    'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed',
    'missing_registration', 'non_n_registration',
    'invalid_n_number', 'serial_conflict'
  )),
  last_failure_phase TEXT CHECK (last_failure_phase IN ('extraction', 'materialization')),
  last_failure_reason_code TEXT
    CHECK (last_failure_reason_code IN (
      'database_error', 'operation_failed', 'faa_lookup_failed', 'faa_listing_not_found',
      'faa_registry_snapshot_unavailable', 'faa_registration_not_found',
      'faa_registration_not_covered', 'faa_ambiguous_registration',
      'faa_registry_aircraft_identity_unavailable', 'faa_aircraft_manufacturer_mismatch',
      'faa_aircraft_model_mismatch', 'faa_canonical_identity_assignment_missing',
      'faa_canonical_identity_assignment_mismatch'
    )),
  extraction_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (extraction_attempt_count >= 0),
  materialization_attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (materialization_attempt_count >= 0),
  extraction_started_at TEXT,
  extraction_completed_at TEXT,
  materialization_started_at TEXT,
  materialization_completed_at TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE (run_id, position),
  UNIQUE (run_id, plugin_submission_id),
  CHECK (length(expected_rendered_html_sha256) = 64),
  CHECK (expected_rendered_html_sha256 = lower(expected_rendered_html_sha256)),
  CHECK (expected_rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (extracted_listing_sha256 IS NULL OR (
    length(extracted_listing_sha256) = 64
    AND extracted_listing_sha256 = lower(extracted_listing_sha256)
    AND extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*'
  )),
  CHECK (
    (extraction_state = 'rejected' AND materialization_state = 'blocked'
      AND terminal_rejection_phase = 'extraction'
      AND terminal_rejection_stage = 'capture_admission'
      AND terminal_rejection_reason_code IN (
        'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
      ))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'rejected'
      AND terminal_rejection_phase = 'materialization'
      AND (
        (terminal_rejection_stage = 'capture_admission'
          AND terminal_rejection_reason_code IN (
            'capture_authentication_failed', 'capture_not_found', 'capture_validation_failed'
          ))
        OR
        (terminal_rejection_stage = 'faa_aircraft_admission'
          AND terminal_rejection_reason_code IN (
            'missing_registration', 'non_n_registration', 'invalid_n_number', 'serial_conflict'
          ))
      ))
    OR
    (extraction_state <> 'rejected' AND materialization_state <> 'rejected'
      AND terminal_rejection_phase IS NULL AND terminal_rejection_stage IS NULL
      AND terminal_rejection_reason_code IS NULL)
  ),
  CHECK (
    (extraction_state = 'failed' AND materialization_state = 'blocked'
      AND last_failure_phase = 'extraction'
      AND last_failure_reason_code IN ('database_error', 'operation_failed'))
    OR
    (extraction_state = 'succeeded' AND materialization_state = 'failed'
      AND last_failure_phase = 'materialization' AND last_failure_reason_code IS NOT NULL)
    OR
    (extraction_state <> 'failed' AND materialization_state <> 'failed'
      AND last_failure_phase IS NULL AND last_failure_reason_code IS NULL)
  ),
  CHECK ((materialization_state = 'succeeded') = (resulting_listing_id IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_sha256 IS NOT NULL)),
  CHECK ((extraction_state = 'succeeded') = (extracted_listing_json IS NOT NULL)),
  CHECK (extraction_state = 'succeeded' OR materialization_state = 'blocked'),
  CHECK (extraction_state <> 'running' OR extraction_started_at IS NOT NULL),
  CHECK (materialization_state <> 'running' OR materialization_started_at IS NOT NULL)
);

CREATE INDEX IF NOT EXISTS idx_listing_replay_run_items_phase
  ON listing_replay_run_items (run_id, extraction_state, materialization_state, position);

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_checkpoint_exact_insert
BEFORE INSERT ON listing_replay_run_items
WHEN NEW.extraction_state = 'succeeded' AND NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  JOIN plugin_installs install ON install.id = submission.plugin_install_id
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.expected_rendered_html_sha256
    AND submission.extracted_listing_json IS NEW.extracted_listing_json
    AND submission.extraction_error IS NULL
    AND julianday(submission.submitted_at) IS NOT NULL
    AND (
      install.revoked_at IS NULL
      OR (
        julianday(install.revoked_at) IS NOT NULL
        AND julianday(submission.submitted_at) <= julianday(install.revoked_at)
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'replay extraction transition does not match its exact checkpoint');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_checkpoint_exact_update
BEFORE UPDATE ON listing_replay_run_items
WHEN NEW.extraction_state = 'succeeded' AND NOT EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  JOIN plugin_installs install ON install.id = submission.plugin_install_id
  WHERE submission.id = NEW.plugin_submission_id
    AND submission.rendered_html_sha256 = NEW.expected_rendered_html_sha256
    AND submission.extracted_listing_json IS NEW.extracted_listing_json
    AND submission.extraction_error IS NULL
    AND julianday(submission.submitted_at) IS NOT NULL
    AND (
      install.revoked_at IS NULL
      OR (
        julianday(install.revoked_at) IS NOT NULL
        AND julianday(submission.submitted_at) <= julianday(install.revoked_at)
      )
    )
)
BEGIN
  SELECT RAISE(ABORT, 'replay extraction transition does not match its exact checkpoint');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_completed_immutable_update
BEFORE UPDATE ON listing_replay_run_items
WHEN OLD.materialization_state = 'succeeded'
BEGIN
  SELECT RAISE(ABORT, 'completed replay item is immutable');
END;

CREATE TRIGGER IF NOT EXISTS listing_replay_run_items_completed_immutable_delete
BEFORE DELETE ON listing_replay_run_items
WHEN OLD.materialization_state = 'succeeded'
BEGIN
  SELECT RAISE(ABORT, 'completed replay item is immutable');
END;

CREATE TABLE IF NOT EXISTS plugin_submission_materialization_receipts (
  plugin_submission_id INTEGER PRIMARY KEY
    REFERENCES plugin_submissions(id) ON DELETE CASCADE,
  aircraft_sale_listing_id INTEGER NOT NULL UNIQUE
    REFERENCES aircraft_sale_listings(id) ON DELETE RESTRICT,
  rendered_html_sha256 TEXT NOT NULL,
  extracted_listing_sha256 TEXT NOT NULL,
  completed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(rendered_html_sha256) = 64),
  CHECK (rendered_html_sha256 = lower(rendered_html_sha256)),
  CHECK (rendered_html_sha256 NOT GLOB '*[^0-9a-f]*'),
  CHECK (length(extracted_listing_sha256) = 64),
  CHECK (extracted_listing_sha256 = lower(extracted_listing_sha256)),
  CHECK (extracted_listing_sha256 NOT GLOB '*[^0-9a-f]*')
);

CREATE TRIGGER IF NOT EXISTS plugin_submission_materialization_receipts_immutable_update
BEFORE UPDATE ON plugin_submission_materialization_receipts
BEGIN
  SELECT RAISE(ABORT, 'replay materialization receipt is immutable');
END;

CREATE TRIGGER IF NOT EXISTS plugin_submission_materialization_receipts_immutable_delete
BEFORE DELETE ON plugin_submission_materialization_receipts
BEGIN
  SELECT RAISE(ABORT, 'replay materialization receipt is immutable');
END;

CREATE UNIQUE INDEX IF NOT EXISTS uq_aircraft_sale_listings_owner_source
  ON aircraft_sale_listings (created_by_user_id, source_url)
  WHERE source_url IS NOT NULL AND length(trim(source_url)) > 0;

CREATE TRIGGER IF NOT EXISTS plugin_submissions_replay_checkpoint_immutable
BEFORE UPDATE ON plugin_submissions
WHEN (
  EXISTS (
    SELECT 1 FROM listing_replay_run_items item
    WHERE item.plugin_submission_id = OLD.id
      AND item.extraction_state = 'succeeded'
  )
  OR EXISTS (
    SELECT 1 FROM plugin_submission_materialization_receipts receipt
    WHERE receipt.plugin_submission_id = OLD.id
  )
) AND (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.plugin_install_id IS OLD.plugin_install_id)
  OR NOT (NEW.source_url IS OLD.source_url)
  OR NOT (NEW.submitted_at IS OLD.submitted_at)
  OR NOT (NEW.rendered_html IS OLD.rendered_html)
  OR NOT (NEW.rendered_html_sha256 IS OLD.rendered_html_sha256)
  OR NOT (NEW.signature_base64 IS OLD.signature_base64)
  OR NOT (NEW.extracted_listing_json IS OLD.extracted_listing_json)
  OR NOT (NEW.extraction_error IS OLD.extraction_error)
  OR NOT (
    NEW.canonical_listing_id IS OLD.canonical_listing_id
    OR (
      OLD.canonical_listing_id IS NULL
      AND NEW.canonical_listing_id IS NOT NULL
      AND NOT EXISTS (
        SELECT 1 FROM plugin_submission_materialization_receipts receipt
        WHERE receipt.plugin_submission_id = OLD.id
      )
      AND EXISTS (
        SELECT 1 FROM aircraft_sale_listings listing
        WHERE listing.id = NEW.canonical_listing_id
          AND listing.created_by_user_id = OLD.user_id
          AND listing.source_url = OLD.source_url
      )
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'replay checkpoint capture is immutable');
END;

CREATE TRIGGER IF NOT EXISTS plugin_installs_replay_identity_immutable
BEFORE UPDATE ON plugin_installs
WHEN EXISTS (
  SELECT 1
  FROM plugin_submissions submission
  WHERE submission.plugin_install_id = OLD.id
    AND (
      EXISTS (
        SELECT 1 FROM listing_replay_run_items item
        WHERE item.plugin_submission_id = submission.id
          AND item.extraction_state = 'succeeded'
      )
      OR EXISTS (
        SELECT 1 FROM plugin_submission_materialization_receipts receipt
        WHERE receipt.plugin_submission_id = submission.id
      )
    )
) AND (
  NOT (NEW.id IS OLD.id)
  OR NOT (NEW.user_id IS OLD.user_id)
  OR NOT (NEW.public_key_base64 IS OLD.public_key_base64)
  OR NOT (NEW.created_at IS OLD.created_at)
  OR NOT (
    NEW.revoked_at IS OLD.revoked_at
    OR (
      OLD.revoked_at IS NULL
      AND NEW.revoked_at IS NOT NULL
      AND julianday(NEW.revoked_at) IS NOT NULL
      AND NOT EXISTS (
        SELECT 1
        FROM plugin_submissions submission
        WHERE submission.plugin_install_id = OLD.id
          AND (
            EXISTS (
              SELECT 1 FROM listing_replay_run_items item
              WHERE item.plugin_submission_id = submission.id
                AND item.extraction_state = 'succeeded'
            )
            OR EXISTS (
              SELECT 1 FROM plugin_submission_materialization_receipts receipt
              WHERE receipt.plugin_submission_id = submission.id
            )
          )
          AND (
            julianday(submission.submitted_at) IS NULL
            OR julianday(submission.submitted_at) > julianday(NEW.revoked_at)
          )
      )
    )
  )
)
BEGIN
  SELECT RAISE(ABORT, 'replay checkpoint plugin identity is immutable');
END;

-- The inventory arbiter is part of the canonical replay state, not bootstrap
-- data. Fail closed if its singleton row or active-run bijection is damaged;
-- never silently heal an already-installed contract.
WITH inventory_lock_is_valid(accepted) AS (
  SELECT
    (SELECT COUNT(*) FROM listing_replay_submission_inventory_lock) = 1
    AND EXISTS (
      SELECT 1
      FROM listing_replay_submission_inventory_lock
      WHERE singleton_id = 1
        AND typeof(singleton_id) = 'integer'
        AND typeof(concurrency_token) = 'integer'
        AND concurrency_token >= 0
        AND (active_run_id IS NULL OR typeof(active_run_id) = 'integer')
    )
    AND NOT EXISTS (
      SELECT 1
      FROM listing_replay_submission_inventory_lock inventory
      LEFT JOIN listing_replay_runs run ON run.id = inventory.active_run_id
      WHERE inventory.active_run_id IS NOT NULL
        AND (run.id IS NULL OR run.status <> 'running')
    )
    AND NOT EXISTS (
      SELECT 1
      FROM listing_replay_runs run
      WHERE run.status = 'running'
        AND run.id IS NOT (
          SELECT active_run_id
          FROM listing_replay_submission_inventory_lock
          WHERE singleton_id = 1
        )
    )
),
duplicate_inventory_guard_rows(row_number) AS (
  VALUES (1), (2)
)
INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
)
SELECT '__listing_replay_submission_inventory_lock_guard__', 1,
       '0000000000000000000000000000000000000000000000000000000000000000',
       'contract-guard'
FROM inventory_lock_is_valid
CROSS JOIN duplicate_inventory_guard_rows
WHERE NOT accepted;

INSERT INTO schema_migration_contracts (
  migration_name, contract_version, contract_fingerprint, installed_at
) VALUES (
  '20260819_listing_replay_runs', 1,
  '3e7c0b39b66e681be397bddbc943c75793b18bac71eacc7324b08a067ef3ff01',
  CURRENT_TIMESTAMP
) ON CONFLICT (migration_name) DO NOTHING;

COMMIT;
