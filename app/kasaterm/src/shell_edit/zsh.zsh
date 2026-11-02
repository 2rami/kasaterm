[[ -o interactive && -o zle ]] || return
(( $+functions[_kasaterm_edit] )) && return

_kasaterm_editor_init() {
  _kasaterm_editor_select=0
  _kasaterm_editor_goal=-1
  _kasaterm_editor_cached_columns=0
  _kasaterm_editor_history=0
  _kasaterm_editor_active
}
_kasaterm_editor_active() {
  if [[ $KEYMAP != main && $KEYMAP != emacs && $KEYMAP != viins ]]; then
    _kasaterm_editor_finish
    return
  fi
  local -i active=1
  (( _kasaterm_editor_select && REGION_ACTIVE && CURSOR != MARK )) && active=2
  printf '\e]777;kasaterm;zle;%d\a' "$active"
}
_kasaterm_editor_finish() {
  printf '\e]777;kasaterm;zle;0\a'
}

_kasaterm_editor_prompt_width() {
  local prompt_text=$PROMPT probe expanded
  [[ -o promptsubst ]] && prompt_text=${(e)prompt_text}
  local -i low=0 high=65536 middle COLUMNS=65536
  prompt_text+=${PREDISPLAY//\%/%%}
  while (( high - low > 1 )); do
    (( middle = (low + high) / 2 ))
    probe="${prompt_text}%${middle}(l.1.0)"
    expanded=${(%)probe}
    if [[ ${expanded[-1]} == 1 ]]; then low=$middle; else high=$middle; fi
  done
  REPLY=$low
}

_kasaterm_editor_positions() {
  if [[ $BUFFER == "$_kasaterm_editor_cached_buffer" && $PROMPT == "$_kasaterm_editor_cached_prompt" &&
        $PREDISPLAY == "$_kasaterm_editor_cached_predisplay" ]] &&
     (( _kasaterm_editor_cached_columns == COLUMNS )); then return; fi
  _kasaterm_editor_cached_buffer=$BUFFER
  _kasaterm_editor_cached_prompt=$PROMPT
  _kasaterm_editor_cached_predisplay=$PREDISPLAY
  _kasaterm_editor_cached_columns=$COLUMNS
  _kasaterm_editor_prompt_width
  local -i cols=${COLUMNS:-80} x y=0 i width
  (( cols < 2 )) && cols=2
  (( x = REPLY % cols ))
  local ch
  _kasaterm_editor_rows=(0)
  _kasaterm_editor_cols=($x)
  for (( i=1; i<=${#BUFFER}; ++i )); do
    ch=${BUFFER[i]}
    if [[ $ch == $'\n' ]]; then
      (( ++y, x=0 ))
    else
      if [[ $ch == $'\t' ]]; then
        (( width = 8 - x % 8 ))
      elif [[ $ch == [[:cntrl:]] ]]; then
        width=2
      else
        width=${(m)#ch}
      fi
      if (( width > 0 && x + width > cols )); then
        (( ++y, x=0 ))
        _kasaterm_editor_rows[i]=$y
        _kasaterm_editor_cols[i]=0
      fi
      (( x += width ))
      if (( x >= cols )); then (( y += x / cols, x %= cols )); fi
    fi
    _kasaterm_editor_rows+=($y)
    _kasaterm_editor_cols+=($x)
  done
}

_kasaterm_editor_motion() {
  local direction=$1
  local -i selecting=$2 selected=0
  if (( selecting )) || [[ $direction != up && $direction != down ]]; then _kasaterm_editor_history=0; fi
  (( _kasaterm_editor_select && REGION_ACTIVE && CURSOR != MARK )) && selected=1
  if (( selecting )); then
    if (( ! _kasaterm_editor_select || ! REGION_ACTIVE )); then MARK=$CURSOR; fi
    REGION_ACTIVE=1
    _kasaterm_editor_select=1
  else
    REGION_ACTIVE=0
    _kasaterm_editor_select=0
    if (( selected )) && [[ $direction == left || $direction == right ]]; then
      if [[ $direction == left ]]; then (( CURSOR = CURSOR < MARK ? CURSOR : MARK ))
      else (( CURSOR = CURSOR > MARK ? CURSOR : MARK )); fi
      return 0
    fi
  fi
  case $direction in
    left) zle .backward-char ;;
    right) zle .forward-char ;;
    word-left) zle .backward-word ;;
    word-right) zle .emacs-forward-word ;;
    start) CURSOR=0 ;;
    end) CURSOR=${#BUFFER} ;;
    paragraph-up)
      local -i previous=$CURSOR
      zle .beginning-of-line
      (( CURSOR != previous || CURSOR == 0 )) || { zle .backward-char; zle .beginning-of-line; } ;;
    paragraph-down)
      local -i previous=$CURSOR
      zle .end-of-line
      (( CURSOR != previous || CURSOR == ${#BUFFER} )) || { zle .forward-char; zle .end-of-line; } ;;
    up|down|line-start|line-end)
      _kasaterm_editor_positions
      local -i row=${_kasaterm_editor_rows[CURSOR+1]} target i best=-1 best_col=-1
      if [[ $direction == up || $direction == down ]]; then
        if [[ $LASTWIDGET != _kasaterm-*up && $LASTWIDGET != _kasaterm-*down ]] ||
           (( _kasaterm_editor_goal < 0 || _kasaterm_editor_width != COLUMNS )); then
          _kasaterm_editor_goal=${_kasaterm_editor_cols[CURSOR+1]}
        fi
        _kasaterm_editor_width=$COLUMNS
        (( target = row - 1 ))
        [[ $direction == down ]] && (( target = row + 1 ))
      else
        target=$row
        if [[ $direction == line-start ]]; then _kasaterm_editor_goal=0
        else _kasaterm_editor_goal=$COLUMNS; fi
      fi
      for (( i=1; i<=${#_kasaterm_editor_rows}; ++i )); do
        if (( _kasaterm_editor_rows[i] == target )); then
          (( best < 0 )) && best=$((i-1))
          if (( _kasaterm_editor_cols[i] <= _kasaterm_editor_goal &&
                _kasaterm_editor_cols[i] >= best_col )); then
            (( best=i-1, best_col=_kasaterm_editor_cols[i] ))
          fi
        fi
      done
      if (( best >= 0 )); then CURSOR=$best
      elif (( ! selecting )) && { [[ -z $BUFFER ]] || (( _kasaterm_editor_history )); }; then
        _kasaterm_editor_history=1
        if [[ $direction == up ]]; then zle .up-history
        elif [[ $direction == down ]]; then zle .down-history; fi
      elif [[ $direction == up ]]; then CURSOR=0
      elif [[ $direction == down ]]; then CURSOR=${#BUFFER}
      fi ;;
  esac
  return 0
}

_kasaterm_editor_delete_selection() {
  if (( _kasaterm_editor_select && REGION_ACTIVE && CURSOR != MARK )); then
    zle .kill-region
    REGION_ACTIVE=0
    _kasaterm_editor_select=0
    return 0
  fi
  (( _kasaterm_editor_select )) && REGION_ACTIVE=0
  _kasaterm_editor_select=0
  return 1
}

_kasaterm_editor_replace() {
  _kasaterm_editor_history=0
  local original="_kasaterm_original_${WIDGET}"
  local -i removed=0
  _kasaterm_editor_delete_selection && removed=1
  if (( removed )) && [[ $WIDGET == *delete* || $WIDGET == *kill* ]]; then return 0; fi
  zle "$original" -- "$@"
}

_kasaterm_editor_copy() {
  (( _kasaterm_editor_select && REGION_ACTIVE && CURSOR != MARK )) || return 0
  zle .copy-region-as-kill
  local encoded
  encoded=$(printf '%s' "$CUTBUFFER" | command base64)
  printf '\e]52;c;%s\a' "${encoded//$'\n'/}"
}

_kasaterm_editor_unhex() {
  local hex=$1 escaped=''
  local -i i
  (( ${#hex} % 2 == 0 )) && [[ $hex != *[^0-9a-f]* ]] || return 1
  for (( i=1; i<=${#hex}; i+=2 )); do escaped+="\\x${hex[i,i+1]}"; done
  printf -v REPLY '%b' "$escaped"
}

_kasaterm_editor_range() {
  local packet start end mode prefix_hex replacement_hex prefix replacement
  zle .bracketed-paste packet || return 0
  (( ${#packet} <= 262144 )) || return 0
  start=${packet%%;*}; packet=${packet#*;}
  end=${packet%%;*}; packet=${packet#*;}
  mode=${packet%%;*}; packet=${packet#*;}
  prefix_hex=${packet%%;*}; replacement_hex=${packet#*;}
  [[ $start == <-> && $end == <-> && ${#start} -le 7 && ${#end} -le 7 ]] || return 0
  [[ $mode == select || $mode == replace || $mode == cut || $mode == suggest-right ||
     $mode == suggest-end || $mode == suggest-word ]] || return 0
  _kasaterm_editor_unhex "$prefix_hex" || return 0
  prefix=$REPLY
  _kasaterm_editor_unhex "$replacement_hex" || return 0
  replacement=$REPLY
  local -i a=$start b=$end length=${#BUFFER}
  if [[ $mode == suggest-* ]]; then
    if [[ $BUFFER == "$prefix" ]] && (( CURSOR == length && (! REGION_ACTIVE || CURSOR == MARK) )); then
      REGION_ACTIVE=0; _kasaterm_editor_select=0; _kasaterm_editor_history=0
      LBUFFER+=$replacement
    else
      case $mode in
        suggest-end) _kasaterm_editor_motion line-end 0 ;;
        suggest-word) _kasaterm_editor_motion word-right 0 ;;
        *) _kasaterm_editor_motion right 0 ;;
      esac
    fi
    return 0
  fi
  _kasaterm_editor_history=0
  REGION_ACTIVE=0
  _kasaterm_editor_select=0
  (( a >= 0 && a < b && b == ${#prefix} )) || return 0
  if (( b > length )); then
    [[ ${prefix[1,length]} == "$BUFFER" && ${prefix[length+1,-1]} != *[^[:space:]]* ]] || return 0
    b=$length
  fi
  # The screen can lag a shell redraw; stale coordinates must never erase a different command.
  (( a < b )) && [[ ${BUFFER[1,b]} == "${prefix[1,b]}" ]] || return 0
  MARK=$a
  CURSOR=$b
  REGION_ACTIVE=1
  _kasaterm_editor_select=1
  if [[ $mode == cut ]]; then _kasaterm_editor_copy; fi
  if [[ $mode != select ]]; then
    _kasaterm_editor_delete_selection
    LBUFFER+=$replacement
  fi
}

_kasaterm_edit() {
  local action=${WIDGET#_kasaterm-}
  case $action in
    select-all) MARK=0; CURSOR=${#BUFFER}; REGION_ACTIVE=1; _kasaterm_editor_select=1; _kasaterm_editor_history=0 ;;
    range) _kasaterm_editor_range ;;
    select-*) _kasaterm_editor_motion "${action#select-}" 1 ;;
    left|right|up|down|word-*|line-*|start|end|paragraph-*) _kasaterm_editor_motion "$action" 0 ;;
    copy) _kasaterm_editor_copy ;;
    cut) _kasaterm_editor_copy; _kasaterm_editor_delete_selection ;;
    newline) _kasaterm_editor_history=0; _kasaterm_editor_delete_selection; LBUFFER+=$'\n' ;;
    erase-start)
      if ! _kasaterm_editor_delete_selection; then
        MARK=$CURSOR; _kasaterm_editor_motion line-start 0
        REGION_ACTIVE=1; _kasaterm_editor_select=1; _kasaterm_editor_delete_selection
      fi ;;
    erase-end)
      if ! _kasaterm_editor_delete_selection; then
        MARK=$CURSOR; _kasaterm_editor_motion line-end 0
        REGION_ACTIVE=1; _kasaterm_editor_select=1; _kasaterm_editor_delete_selection
      fi ;;
    undo|redo) REGION_ACTIVE=0; _kasaterm_editor_select=0; _kasaterm_editor_history=0; zle ".$action" ;;
    cancel)
      if (( _kasaterm_editor_select )); then REGION_ACTIVE=0; _kasaterm_editor_select=0
      elif [[ $KEYMAP == viins || "$(bindkey -lL main)" == *viins* ]]; then zle .vi-cmd-mode; fi ;;
  esac
  return 0
}

_kasaterm_editor_bind() {
  local keys=$1 action=$2 map
  zle -N "_kasaterm-$action" _kasaterm_edit
  for map in emacs viins; do bindkey -M "$map" "$keys" "_kasaterm-$action"; done
}

local _kt_widget _kt_arrow _kt_direction
for _kt_widget in self-insert self-insert-unmeta backward-delete-char delete-char \
  backward-kill-word kill-word kill-line kill-whole-line backward-kill-line bracketed-paste; do
  zle -A "$_kt_widget" "_kasaterm_original_$_kt_widget" 2>/dev/null || continue
  zle -N "$_kt_widget" _kasaterm_editor_replace
done
for _kt_arrow _kt_direction in A up B down C right D left; do
  _kasaterm_editor_bind $'\e['"$_kt_arrow" "$_kt_direction"
  _kasaterm_editor_bind $'\eO'"$_kt_arrow" "$_kt_direction"
  _kasaterm_editor_bind $'\e[1;2'"$_kt_arrow" "select-$_kt_direction"
done
_kasaterm_editor_bind $'\e[1;4D' select-word-left
_kasaterm_editor_bind $'\e[1;4C' select-word-right
_kasaterm_editor_bind $'\eb' word-left
_kasaterm_editor_bind $'\ef' word-right
_kasaterm_editor_bind $'\e[1;5D' word-left
_kasaterm_editor_bind $'\e[1;5C' word-right
_kasaterm_editor_bind $'\e[1;6D' select-word-left
_kasaterm_editor_bind $'\e[1;6C' select-word-right
_kasaterm_editor_bind $'\e[1;3A' paragraph-up
_kasaterm_editor_bind $'\e[1;3B' paragraph-down
_kasaterm_editor_bind $'\e[1;4A' select-paragraph-up
_kasaterm_editor_bind $'\e[1;4B' select-paragraph-down
_kasaterm_editor_bind $'\e[1;9D' line-start
_kasaterm_editor_bind $'\e[1;9C' line-end
_kasaterm_editor_bind $'\e[1;9A' start
_kasaterm_editor_bind $'\e[1;9B' end
_kasaterm_editor_bind $'\e[1;10D' select-line-start
_kasaterm_editor_bind $'\e[1;10C' select-line-end
_kasaterm_editor_bind $'\e[1;10A' select-start
_kasaterm_editor_bind $'\e[1;10B' select-end
_kasaterm_editor_bind $'\e[97;9u' select-all
_kasaterm_editor_bind $'\e[99;9u' copy
_kasaterm_editor_bind $'\e[120;9u' cut
_kasaterm_editor_bind $'\e[122;9u' undo
_kasaterm_editor_bind $'\e[122;10u' redo
_kasaterm_editor_bind $'\e[13;2u' newline
_kasaterm_editor_bind $'\e[13;3u' newline
_kasaterm_editor_bind $'\e[127;9u' erase-start
_kasaterm_editor_bind $'\e[3;9~' erase-end
_kasaterm_editor_bind $'\e[27;1u' cancel
_kasaterm_editor_bind $'\e[9001~' range
for _kt_widget in emacs viins; do
  bindkey -M "$_kt_widget" $'\e[3~' delete-char
  bindkey -M "$_kt_widget" $'\e[3;5~' kill-word
  bindkey -M "$_kt_widget" $'\e[3;3~' kill-word
done
_kasaterm_editor_bind $'\e[H' line-start
_kasaterm_editor_bind $'\e[F' line-end
_kasaterm_editor_bind $'\e[1;2H' select-line-start
_kasaterm_editor_bind $'\e[1;2F' select-line-end
unset _kt_widget _kt_arrow _kt_direction
autoload -Uz add-zle-hook-widget
add-zle-hook-widget line-init _kasaterm_editor_init
add-zle-hook-widget line-finish _kasaterm_editor_finish
add-zle-hook-widget line-pre-redraw _kasaterm_editor_active
add-zle-hook-widget keymap-select _kasaterm_editor_active
add-zle-hook-widget isearch-update _kasaterm_editor_finish
add-zle-hook-widget isearch-exit _kasaterm_editor_active
