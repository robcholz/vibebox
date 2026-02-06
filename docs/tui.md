# TUI

The end-user ui for the vibebox

## TUI header

- This is a welcome header.
- Shows text: "Welcome to Vibebox vX.X.XX"
- Shows the ASCII banner
- An outlined box
    - Shows the current directory
    - Shows current vm version and max memory, cpu cores
- The position is flex, so this will move with the VM terminal history.

## Terminal Area

- Shows all the VM terminal history

## Vibebox input area

- A text input area, user can input text in it, it can vertically expand depending on the text length, by default it
  is a line high.
- it should be able to switch to auto-completion mode, which will display a list of available commands. When in this mode, the bottom
  status bar will disappear. The auto completions are displayed right below the text input area.

## Bottom status bar

- Display texts in gray, a line high. on the left it shows `:help` for help.