# Lambda Simulator Freeze/Thaw Demo
# Shows the full Lambda lifecycle with real process freeze/thaw using SIGSTOP/SIGCONT

Output freeze_demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 900
Set Height 700
Set Padding 20
Set Theme "Catppuccin Mocha"
Set TypingSpeed 30ms
Set PlaybackSpeed 0.5

Type "# Lambda Simulator - Process Freeze/Thaw Demo"
Enter
Sleep 1s

Type "# Real SIGSTOP/SIGCONT process freezing like AWS Lambda"
Enter
Sleep 1s

Type "DEMO_INTERACTIVE=1 cargo test -p lambda-simulator --test freeze_demo_test --release -q -- --nocapture --ignored"
Enter

# Wait for init to complete
Sleep 4s

# Advance past init → first invocation
Type "# Init complete - sending first invocation (cold start)"
Enter
Sleep 3s

# Advance past first invocation → freeze
Type "# Invocation complete - now freezing processes with SIGSTOP"
Enter
Sleep 3s

# Advance past freeze → second invocation (thaw)
Type "# Processes frozen - sending second invocation (triggers SIGCONT thaw)"
Enter
Sleep 3s

# Advance past second invocation → third invocation
Type "# Warm invocation complete - sending third invocation"
Enter
Sleep 3s

# Advance past third invocation → shutdown
Type "# All invocations done - initiating graceful shutdown"
Enter
Sleep 4s

# Wait for telemetry summary to display
Sleep 4s
