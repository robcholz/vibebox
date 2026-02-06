# New Features

## Priority 1

### Session management

1. each session is a folder
2. by default, there should be a reference count for each vm, if count is 0, shutdown in DEFAULT_EXPECT_TIMEOUT.
3. You can also use command inside a session to never allow it to auto shutdown
4. you can use commands to resume a session

### explain (Shows all the mounts/network/visibility)

1. you can display all the mounts & network activity.
2. you can set disable to disable a network connection.
3. use a config file in .vibebox/config.toml to config it

## Priority 2

### Port Forwarding

1. Port will be auto forwarded to the host
