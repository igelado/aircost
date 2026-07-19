import tempfile
import unittest
from pathlib import Path

from aircost.webapp.database import DEVELOPER_EMAIL, connect_database, initialize_database


class WebappDatabaseTests(unittest.TestCase):
    def test_initializes_schema_and_seed_data(self):
        with tempfile.TemporaryDirectory() as directory:
            database_path = Path(directory) / "aircost.sqlite3"
            initialize_database(database_path)

            with connect_database(database_path) as connection:
                user = connection.execute(
                    "SELECT email FROM users WHERE auth_subject = 'developer'"
                ).fetchone()
                profiles = connection.execute(
                    "SELECT COUNT(*) AS count FROM depreciation_profiles"
                ).fetchone()

        self.assertEqual(user["email"], DEVELOPER_EMAIL)
        self.assertGreaterEqual(profiles["count"], 1)


if __name__ == "__main__":
    unittest.main()
