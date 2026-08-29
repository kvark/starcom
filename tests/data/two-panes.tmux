%begin 1 1 0
%end 1 1 0
%session-changed $1 starcom-test
%output %1 Starcom shell\015\012$ cargo test\015\012
%output %2 \033[32mready\033[0m\015\012
%output %1 working\015\033[2Kdone\015\012
%output %2 café 界\015\012
%exit detached
