-- Since this service is intended to be used for files too large to be sent
-- directly in a chat message, it should smooth over transient network issues.
module Main exposing (..)

import Browser
import Browser.Navigation as Nav
import Html exposing (Html)
import Html.Events as Events
import Html.Attributes as Attr
import Json.Decode as JsonDecode
import Url

-- Main
main : Program () Model Msg
main = Browser.application
       { init = init
       , view = view
       , update = update
       , subscriptions = subscriptions
       , onUrlChange = UrlChanged
       , onUrlRequest = LinkClicked
       }

-- Model
type alias Model = {}

init : () -> Url.Url -> Nav.Key -> (Model, Cmd Msg)
init flags url key = (Model, Cmd.none)

-- Update
type Msg
    = LinkClicked Browser.UrlRequest
    | UrlChanged Url.Url

update : Msg -> Model -> (Model, Cmd Msg)
update msg model = (model, Cmd.none)

-- Subscriptions
subscriptions : Model -> Sub Msg
subscriptions _ = Sub.none

-- View
view : Model -> Browser.Document Msg
view model =
    { title = "Fileshare"
    , body =
        [ Html.text "I am a single page web app!" ]
    }
