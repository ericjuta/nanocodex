"""Fast local verifier adapters that preserve benchmark assertions."""

import shlex
from typing import Any, override

from harbor.models.trial.paths import EnvironmentPaths
from harbor.models.verifier.result import VerifierResult
from harbor.utils.env import resolve_env_vars
from harbor.verifier.verifier import Verifier


class PytestVerifier(Verifier):
    """Upload canonical tests, then run preinstalled pytest directly."""

    def __init__(self, *args: Any, test_file: str, **kwargs: Any) -> None:
        super().__init__(*args, **kwargs)
        self._test_file = test_file

    @override
    async def verify(self) -> VerifierResult:
        if not self.environment.capabilities.mounted:
            raise RuntimeError("PytestVerifier requires a mounted environment")

        environment_paths = EnvironmentPaths.for_os(self.environment.os)
        test_source_dirs, _, _ = self._resolve_tests()
        for source_dir in test_source_dirs:
            await self.environment.upload_dir(
                source_dir=source_dir,
                target_dir=str(environment_paths.tests_dir),
            )

        test_path = environment_paths.tests_dir / self._test_file
        command = "\n".join(
            (
                "status=0",
                "python -m pytest "
                f"--ctrf {environment_paths.verifier_dir}/ctrf.json "
                f"{shlex.quote(str(test_path))} -rA "
                f"> {environment_paths.verifier_dir}/test-stdout.txt 2>&1 "
                "|| status=$?",
                f'if [ "$status" -eq 0 ]; then echo 1; else echo 0; fi '
                f"> {environment_paths.reward_text_path}",
            )
        )
        merged_env = {
            **self.task.config.verifier.env,
            **(self.verifier_env or {}),
            **self.override_env,
        }
        await self.environment.exec(
            command=command,
            env=resolve_env_vars(merged_env) if merged_env else None,
        )
        return VerifierResult(rewards=self._parse_reward_text())
