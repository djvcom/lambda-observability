# Lambda Simulator Demo
# Shows the full Lambda lifecycle with colourised output

Output demo.gif

Set Shell "bash"
Set FontSize 14
Set Width 900
Set Height 700
Set Padding 20
Set Theme "Catppuccin Mocha"
Set TypingSpeed 40ms

Type "# Lambda Simulator - Full Lambda Lifecycle Demo"
Enter
Sleep 500ms

Type "cargo test -p lambda-simulator --test demo_test -- --nocapture 2>&1 | grep -v 'Compiling\|Finished\|Running.*target/debug'"
Enter
Sleep 10s

# Pause at the end so viewers can read the output
Sleep 5s
