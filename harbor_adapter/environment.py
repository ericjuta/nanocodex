"""Docker optimizations for the local Harbor loop."""

from pathlib import Path
from typing import Any, override

from harbor.environments.docker.docker import DockerEnvironment
from harbor.environments.docker.utils import (
    default_docker_platform,
    ensure_docker_image_built,
)


class FastDockerEnvironment(DockerEnvironment):
    """Cache native task and verifier images and stop Compose immediately."""

    def __init__(
        self,
        *args: Any,
        verifier_dockerfile: str | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init__(*args, **kwargs)
        self._verifier_dockerfile = (
            Path(verifier_dockerfile).resolve() if verifier_dockerfile else None
        )

    @override
    async def start(self, force_build: bool) -> None:
        if self._verifier_dockerfile is not None:
            task_dockerfile = self.environment_dir / "Dockerfile"
            if not task_dockerfile.is_file():
                raise RuntimeError(
                    "verifier image caching requires the task's environment/Dockerfile"
                )

            platform = await default_docker_platform()
            task_image = await ensure_docker_image_built(
                docker_name=f"harness/{self.environment_name}-task",
                docker_build_context=self.environment_dir,
                dockerfile_path=task_dockerfile,
                build_args={},
                platform=platform,
                logger=self.logger,
            )
            image = await ensure_docker_image_built(
                docker_name=f"harness/{self.environment_name}-eval",
                docker_build_context=self._verifier_dockerfile.parent,
                dockerfile_path=self._verifier_dockerfile,
                build_args={"BASE_IMAGE": task_image},
                platform=platform,
                logger=self.logger,
            )
            self.task_env_config.docker_image = image
            self._env_vars.prebuilt_image_name = image
            force_build = False
        await super().start(force_build)

    @override
    async def _run_docker_compose_command(
        self, command: list[str], *args: Any, **kwargs: Any
    ) -> Any:
        if command and command[0] in {"down", "stop"}:
            command = [*command, "--timeout", "0"]
        return await super()._run_docker_compose_command(command, *args, **kwargs)
