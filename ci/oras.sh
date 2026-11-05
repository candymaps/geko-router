# `oras`, the client of OCI registries that publishes the country packs (sourced by ci/packs.sh). It
# runs from its image when it is not installed. With ~/.config/oras/config.json
# (`oras login --registry-config ~/.config/oras/config.json ghcr.io`), it uses those credentials: the
# store of Docker Desktop does not take those of oras from WSL.
oras() {
    local config=$HOME/.config/oras/config.json
    if type -P oras > /dev/null; then
        if [ -f "$config" ]; then
            command oras "$1" --registry-config "$config" "${@:2}"
        else
            command oras "$@"
        fi
    else
        docker run --rm --network host -v "$PWD:$PWD" -w "$PWD" -v "${DOCKER_CONFIG:-$HOME/.docker}:/root/.docker:ro" \
            ghcr.io/oras-project/oras:v1.2.2 "$@"
    fi
}
