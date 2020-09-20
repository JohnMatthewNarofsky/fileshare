module Main exposing (..)
import Browser
import Html exposing (Html)
import Html.Events as Events

main = Browser.sandbox { init = 0, update = update, view = view }

type Msg = Increment | Decrement

update msg model =
    case msg of
        Increment -> model + 1
        Decrement -> model - 1

view model = Html.div []
             [ Html.button [ Events.onClick Decrement ] [ Html.text "-" ]
             , Html.div [] [ Html.text (String.fromInt model) ]
             ,  Html.button [ Events.onClick Increment ] [ Html.text "+" ]
             ]
