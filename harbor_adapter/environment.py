"""Docker optimizations for the local Harbor loop."""

from pathlib import Path
from typing import Any, override

from harbor.environments.docker.docker import DockerEnvironment
from harbor.environments.docker.utils import (
    default_docker_platform,
    ensure_docker_image_built,
)


class FastDockerEnvironment(DockerEnvironment):
    """Cache a development image and skip Compose's teardown grace period."""

    def __init__(
        self,
        *args: Any,
        cached_dockerfile: str | None = None,
        **kwargs: Any,
    ) -> None:
        super().__init__(*args, **kwargs)
        self._cached_dockerfile = (
            Path(cached_dockerfile).resolve() if cached_dockerfile else None
        )

    @override
    async def start(self, force_build: bool) -> None:
        if self._cached_dockerfile is not None:
            image = await ensure_docker_image_built(
                docker_name=f"harness/{self.environment_name}-eval",
                docker_build_context=self.environment_dir,
                dockerfile_path=self._cached_dockerfile,
                build_args={},
                platform=await default_docker_platform(),
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
