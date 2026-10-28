#!/usr/bin/env sh
set -eu

# Uninstalls Lin

check_remaining_installations() {
    platform="$(uname -s)"
    if [ "$platform" = "Darwin" ]; then
        # Check for any Lin variants in /Applications
        remaining=$(ls -d /Applications/Lin*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    else
        # Check for any Lin variants in ~/.local
        remaining=$(ls -d "$HOME/.local/lin"*.app 2>/dev/null | wc -l)
        [ "$remaining" -eq 0 ]
    fi
}

prompt_remove_preferences() {
    printf "Do you want to keep your Lin preferences? [Y/n] "
    read -r response
    case "$response" in
        [nN]|[nN][oO])
            rm -rf "$HOME/.config/lin"
            echo "Preferences removed."
            ;;
        *)
            echo "Preferences kept."
            ;;
    esac
}

main() {
    platform="$(uname -s)"
    channel="${LIN_CHANNEL:-stable}"

    if [ "$platform" = "Darwin" ]; then
        platform="macos"
    elif [ "$platform" = "Linux" ]; then
        platform="linux"
    else
        echo "Unsupported platform $platform"
        exit 1
    fi

    "$platform"

    echo "Lin has been uninstalled"
}

linux() {
    suffix=""
    if [ "$channel" != "stable" ]; then
        suffix="-$channel"
    fi

    appid=""
    db_suffix="stable"
    case "$channel" in
      stable)
        appid="local.lin.Lin"
        db_suffix="stable"
        ;;
      nightly)
        appid="local.lin.Lin-Nightly"
        db_suffix="nightly"
        ;;
      preview)
        appid="local.lin.Lin-Preview"
        db_suffix="preview"
        ;;
      dev)
        appid="local.lin.Lin-Dev"
        db_suffix="dev"
        ;;
      *)
        echo "Unknown release channel: ${channel}. Using stable app ID."
        appid="local.lin.Lin"
        db_suffix="stable"
        ;;
    esac

    # Remove the app directory
    rm -rf "$HOME/.local/lin$suffix.app"

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/lin"

    # Remove the .desktop file
    rm -f "$HOME/.local/share/applications/${appid}.desktop"

    # Remove the database directory for this channel
    rm -rf "$HOME/.local/share/lin/db/0-$db_suffix"

    # Remove socket file
    rm -f "$HOME/.local/share/lin/lin-$db_suffix.sock"

    # Remove the entire Lin directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/.local/share/lin"
        prompt_remove_preferences
    fi

    rm -rf $HOME/.lin_server
}

macos() {
    app="Lin.app"
    db_suffix="stable"
    app_id="local.lin.Lin"
    case "$channel" in
      nightly)
        app="Lin Nightly.app"
        db_suffix="nightly"
        app_id="local.lin.Lin-Nightly"
        ;;
      preview)
        app="Lin Preview.app"
        db_suffix="preview"
        app_id="local.lin.Lin-Preview"
        ;;
      dev)
        app="Lin Dev.app"
        db_suffix="dev"
        app_id="local.lin.Lin-Dev"
        ;;
    esac

    # Remove the app bundle
    if [ -d "/Applications/$app" ]; then
        rm -rf "/Applications/$app"
    fi

    # Remove the binary symlink
    rm -f "$HOME/.local/bin/lin"

    # Remove the database directory for this channel
    rm -rf "$HOME/Library/Application Support/Lin/db/0-$db_suffix"

    # Remove app-specific files and directories
    rm -rf "$HOME/Library/Application Support/com.apple.sharedfilelist/com.apple.LSSharedFileList.ApplicationRecentDocuments/$app_id.sfl"*
    rm -rf "$HOME/Library/Caches/$app_id"
    rm -rf "$HOME/Library/HTTPStorages/$app_id"
    rm -rf "$HOME/Library/Preferences/$app_id.plist"
    rm -rf "$HOME/Library/Saved Application State/$app_id.savedState"

    # Remove the entire Lin directory if no installations remain
    if check_remaining_installations; then
        rm -rf "$HOME/Library/Application Support/Lin"
        rm -rf "$HOME/Library/Logs/Lin"

        prompt_remove_preferences
    fi

    rm -rf $HOME/.lin_server
}

main "$@"
