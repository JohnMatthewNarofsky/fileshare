const address = "wss://home.greenjaffaco.com:9000";
const reload_key = 'fileshare-dev-reload-token';
const error_class = 'reload-error';
var socket = new WebSocket(address);
function milliseconds(t) {
	return new Promise(resolve => {
		setTimeout(() => {
			resolve('resolved');
		}, t);
	});
}

function make_error(text) {
	let elem = document.createElement('pre');
	elem.appendChild(document.createTextNode(text));
	elem.classList.add(error_class);
	return elem;
}

function on_message(event) {
	let data = JSON.parse(event.data);
	console.log(data);
	if (data.RefreshPage) {
		let refresh_token = JSON.parse(localStorage.getItem(reload_key));
		if (data.RefreshPage !== refresh_token) {
			localStorage.setItem(reload_key, JSON.stringify(data.RefreshPage));
			location.reload();
		} else {
			console.log("Don't need to reload again.");
		}
	} else if (data.DisplayError) {
		// Remove previous error messages,
		// since we know that the server will regenerate them if necessary.
		for (const elem of document.getElementsByClassName(error_class)) {
			document.body.removeChild(elem);
		}
		document.body.appendChild(make_error(data.DisplayError));
	}
}
function on_error(event) {}

async function on_close(event) {
	console.log("Reload server connection closed. Retrying...");
	socket.close();
	socket = new WebSocket(address);
	init_socket(socket);
}

function on_open(event) {
	console.log("Socket open.");
}

function init_socket(socket) {
	socket.addEventListener('close', on_close);
	socket.addEventListener('message', on_message);
	socket.addEventListener('error', on_error);
	socket.addEventListener('open', on_open);
}

init_socket(socket);
